use std::io::{Result, Write};

use crate::{
    analysis::{self, functions::Visibility, special_functions::FunctionType},
    version::Version,
    Env,
};

use super::general::version_condition;

pub(super) fn generate(
    w: &mut dyn Write,
    env: &Env,
    function: &analysis::functions::Info,
    specials: &analysis::special_functions::Infos,
    scope_version: Option<Version>,
) -> Result<bool> {
    if let Some(special) = specials.functions().get(&function.glib_name) {
        match special.type_ {
            FunctionType::StaticStringify => {
                generate_static_to_str(w, env, function, scope_version)
            }
        }
        .map(|()| true)
    } else {
        Ok(false)
    }
}

pub(super) fn generate_static_to_str(
    w: &mut dyn Write,
    env: &Env,
    function: &analysis::functions::Info,
    scope_version: Option<Version>,
) -> Result<()> {
    writeln!(w)?;
    let version = Version::if_stricter_than(function.version, scope_version);
    version_condition(w, env, None, version, false, 1)?;

    let visibility = match function.visibility {
        Visibility::Public => "pub ",
        _ => "",
    };

    writeln!(
        w,
        "\
\t{visibility}fn {rust_fn_name}<'a>(self) -> &'a str {{
\t\tunsafe {{
\t\t\tCStr::from_ptr(
\t\t\t\t{ns}::{glib_fn_name}(self.into_glib())
\t\t\t\t\t.as_ref()
\t\t\t\t\t.expect(\"{glib_fn_name} returned NULL\"),
\t\t\t)
\t\t\t.to_str()
\t\t\t.expect(\"{glib_fn_name} returned an invalid string\")
\t\t}}
\t}}",
        visibility = visibility,
        rust_fn_name = function.codegen_name(),
        ns = env.main_sys_crate_name(),
        glib_fn_name = function.glib_name,
    )?;

    Ok(())
}
