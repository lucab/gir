use std::collections::HashMap;

use crate::config;
use crate::config::parameter_matchable::ParameterMatchable;
use crate::env::Env;
use crate::library::{self, TypeId, ParameterScope};
use crate::nameutil;
use super::conversion_type::ConversionType;
use super::out_parameters::can_as_return;
use super::override_string_type::override_string_type_parameter;
use super::rust_type::rust_type;
use super::ref_mode::RefMode;
use crate::traits::IntoString;

//TODO: remove unused fields
#[derive(Clone, Debug)]
pub struct RustParameter {
    pub ind_c: usize, //index in `Vec<CParameter>`
    pub name: String,
    pub typ: TypeId,
    pub allow_none: bool,
}

#[derive(Clone, Debug)]
pub struct CParameter {
    pub name: String,
    pub typ: TypeId,
    pub c_type: String,
    pub instance_parameter: bool,
    pub direction: library::ParameterDirection,
    pub nullable: library::Nullable,
    pub transfer: library::Transfer,
    pub caller_allocates: bool,
    pub is_error: bool,
    pub scope: ParameterScope,
    /// Index of the user data parameter associated with the callback.
    pub user_data_index: Option<usize>,
    /// Index of the destroy notification parameter associated with the callback.
    pub destroy_index: Option<usize>,

    //analysis fields
    pub ref_mode: RefMode,
}

#[derive(Clone, Debug)]
pub enum TransformationType {
    ToGlibDirect { name: String },
    ToGlibScalar {
        name: String,
        nullable: library::Nullable,
    },
    ToGlibPointer {
        name: String,
        instance_parameter: bool,
        transfer: library::Transfer,
        ref_mode: RefMode,
        //filled by functions
        to_glib_extra: String,
        explicit_target_type: String,
        pointer_cast: String,
        in_trait: bool,
        nullable: bool,
    },
    ToGlibBorrow,
    ToGlibUnknown { name: String },
    Length {
        array_name: String,
        array_length_name: String,
        array_length_type: String,
    },
    IntoRaw(String),
    ToSome(String),
}

impl TransformationType {
    pub fn is_to_glib(&self) -> bool {
        use self::TransformationType::*;
        match *self {
            ToGlibDirect { .. } |
            ToGlibScalar { .. } |
            ToGlibPointer { .. } |
            ToGlibBorrow |
            ToGlibUnknown { .. } |
            ToSome(_) |
            IntoRaw(_) => true,
            _ => false,
        }
    }

    pub fn set_to_glib_extra(&mut self, to_glib_extra_: &str) {
        if let TransformationType::ToGlibPointer {
            ref mut to_glib_extra,
            ..
        } = *self
        {
            *to_glib_extra = to_glib_extra_.to_owned();
        }
    }
}

#[derive(Clone, Debug)]
pub struct Transformation {
    pub ind_c: usize,            //index in `Vec<CParameter>`
    pub ind_rust: Option<usize>, //index in `Vec<RustParameter>`
    pub transformation_type: TransformationType,
}

#[derive(Clone, Default, Debug)]
pub struct Parameters {
    pub rust_parameters: Vec<RustParameter>,
    pub c_parameters: Vec<CParameter>,
    pub transformations: Vec<Transformation>,
}

impl Parameters {
    fn new(capacity: usize) -> Parameters {
        Parameters {
            rust_parameters: Vec::with_capacity(capacity),
            c_parameters: Vec::with_capacity(capacity),
            transformations: Vec::with_capacity(capacity),
        }
    }

    pub fn analyze_return(&mut self, env: &Env, ret: &Option<library::Parameter>) {
        let array_length = if let Some(array_length) = ret.as_ref().and_then(|r| r.array_length) {
            array_length
        } else {
            return;
        };

        let ind_c = array_length as usize;

        let par = if let Some(par) = self.c_parameters.get(ind_c) {
            par
        } else {
            return;
        };

        let transformation = Transformation {
            ind_c,
            ind_rust: None,
            transformation_type: get_length_type(env, "", &par.name, par.typ),
        };
        self.transformations.push(transformation);
    }
}

#[allow(clippy::useless_let_if_seq)]
pub fn analyze(
    env: &Env,
    function_parameters: &[library::Parameter],
    configured_functions: &[&config::functions::Function],
    disable_length_detect: bool,
    async_func: bool,
    in_trait: bool,
) -> Parameters {
    let mut parameters = Parameters::new(function_parameters.len());

    // Map: length argument position => array name
    let array_lengths: HashMap<u32, String> = function_parameters
        .iter()
        .filter_map(|p| p.array_length.map(|pos| (pos, p.name.clone())))
        .collect();

    for (pos, par) in function_parameters.iter().enumerate() {
        let name = if par.instance_parameter {
            par.name.clone()
        } else {
            nameutil::mangle_keywords(&*par.name).into_owned()
        };

        let configured_parameters = configured_functions.matched_parameters(&name);

        let c_type = par.c_type.clone();
        let typ = override_string_type_parameter(env, par.typ, &configured_parameters);

        let ind_c = parameters.c_parameters.len();
        let mut ind_rust = Some(parameters.rust_parameters.len());

        let mut add_rust_parameter = match par.direction {
            library::ParameterDirection::In | library::ParameterDirection::InOut => true,
            library::ParameterDirection::Return => false,
            library::ParameterDirection::Out => !can_as_return(env, par) && !async_func,
        };

        if async_func && async_param_to_remove(&par.name) {
            add_rust_parameter = false;
        }

        let mut array_name = configured_parameters
            .iter()
            .filter_map(|p| p.length_of.as_ref())
            .next();
        if array_name.is_none() {
            array_name = array_lengths.get(&(pos as u32))
        }
        if array_name.is_none() && !disable_length_detect {
            array_name = detect_length(env, pos, par, function_parameters);
        }
        if let Some(array_name) = array_name {
            let array_name = nameutil::mangle_keywords(&array_name[..]);
            add_rust_parameter = false;

            let transformation = Transformation {
                ind_c,
                ind_rust: None,
                transformation_type: get_length_type(env, &array_name, &par.name, typ),
            };
            parameters.transformations.push(transformation);
        }

        let mut caller_allocates = par.caller_allocates;
        let mut transfer = par.transfer;
        let conversion = ConversionType::of(env, typ);
        if conversion == ConversionType::Direct || conversion == ConversionType::Scalar {
            //For simply types no reason to have these flags
            caller_allocates = false;
            transfer = library::Transfer::None;
        }

        let immutable = configured_parameters.iter().any(|p| p.constant);
        let ref_mode = RefMode::without_unneeded_mut(env, par, immutable,
                                                     in_trait && par.instance_parameter);

        let nullable_override = configured_parameters
            .iter()
            .filter_map(|p| p.nullable)
            .next();
        let nullable = nullable_override.unwrap_or(par.nullable);

        let c_par = CParameter {
            name: name.clone(),
            typ,
            c_type,
            instance_parameter: par.instance_parameter,
            direction: par.direction,
            transfer,
            caller_allocates,
            nullable,
            ref_mode,
            is_error: par.is_error,
            scope: par.scope,
            user_data_index: par.closure,
            destroy_index: par.destroy,
        };
        parameters.c_parameters.push(c_par);

        let data_param_name = "user_data";
        let callback_param_name = "callback";

        if add_rust_parameter {
            let rust_par = RustParameter {
                name: name.clone(),
                typ,
                ind_c,
                allow_none: par.allow_none,
            };
            parameters.rust_parameters.push(rust_par);
        } else {
            ind_rust = None;
        }

        let mut trans_nullable = false;
        let type_ = env.type_(par.typ);
        let to_glib_extra = if par.instance_parameter || !*nullable ||
                               (!type_.is_interface() && !type_.is_class()) {
            String::new()
        } else {
            trans_nullable = *nullable;
            if !type_.is_final_type() {
                ".as_ref()".to_owned()
            } else {
                String::new()
            }
        };

        let transformation_type = match ConversionType::of(env, typ) {
            ConversionType::Direct => TransformationType::ToGlibDirect { name },
            ConversionType::Scalar => TransformationType::ToGlibScalar {
                name,
                nullable,
            },
            ConversionType::Pointer => TransformationType::ToGlibPointer {
                name,
                instance_parameter: par.instance_parameter,
                transfer,
                ref_mode,
                to_glib_extra,
                explicit_target_type: String::new(),
                pointer_cast: String::new(),
                in_trait,
                nullable: trans_nullable,
            },
            ConversionType::Borrow => TransformationType::ToGlibBorrow,
            ConversionType::Unknown => TransformationType::ToGlibUnknown { name },
        };

        let mut transformation = Transformation {
            ind_c,
            ind_rust,
            transformation_type,
        };
        let mut transformation_type = None;
        match transformation.transformation_type {
            TransformationType::ToGlibDirect { ref name, .. } |
            TransformationType::ToGlibUnknown { ref name, .. } => {
                if async_func && name == callback_param_name {
                    // Remove the conversion of callback for async functions.
                    transformation_type = Some(TransformationType::ToSome(name.clone()));
                }
            }
            TransformationType::ToGlibPointer { ref name, .. } => {
                if async_func && name == data_param_name {
                    // Do the conversion of user_data for async functions.
                    // In async functions, this argument is used to send the callback.
                    transformation_type = Some(TransformationType::IntoRaw(name.clone()));
                }
            }
            _ => (),
        }
        if let Some(transformation_type) = transformation_type {
            transformation.transformation_type = transformation_type;
        }
        parameters.transformations.push(transformation);
    }

    parameters
}

fn get_length_type(
    env: &Env,
    array_name: &str,
    length_name: &str,
    length_typ: TypeId,
) -> TransformationType {
    let array_length_type = rust_type(env, length_typ).into_string();
    TransformationType::Length {
        array_name: array_name.to_string(),
        array_length_name: length_name.to_string(),
        array_length_type,
    }
}

fn detect_length<'a>(
    env: &Env,
    pos: usize,
    par: &library::Parameter,
    parameters: &'a [library::Parameter],
) -> Option<&'a String> {
    if !is_length(par) {
        return None;
    }

    let array = parameters
        .get(pos - 1)
        .and_then(|p| if has_length(env, p.typ) {
            Some(p)
        } else {
            None
        });
    array.map(|p| &p.name)
}

fn is_length(par: &library::Parameter) -> bool {
    if par.direction != library::ParameterDirection::In {
        return false;
    }

    let len = par.name.len();
    if len >= 3 && &par.name[len - 3..len] == "len" {
        return true;
    }
    if par.name.find("length").is_some() {
        return true;
    }

    false
}

fn has_length(env: &Env, typ: TypeId) -> bool {
    use crate::library::Type;
    let typ = env.library.type_(typ);
    match *typ {
        Type::Fundamental(fund) => {
            use crate::library::Fundamental::*;
            match fund {
                Utf8 => true,
                Filename => true,
                OsString => true,
                _ => false,
            }
        }
        Type::CArray(..) |
        Type::FixedArray(..) |
        Type::Array(..) |
        Type::PtrArray(..) |
        Type::List(..) |
        Type::SList(..) |
        Type::HashTable(..) => true,
        Type::Alias(ref alias) => has_length(env, alias.typ),
        _ => false,
    }
}

pub fn async_param_to_remove(name: &str) -> bool {
    name == "user_data" || name.ends_with("data") // FIXME: use async indexes instead
}
