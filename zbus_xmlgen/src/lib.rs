use snakecase::ascii::to_snakecase;
use std::{
    error::Error,
    fmt::{Display, Formatter, Write},
    process::{Command, Stdio},
};

use zbus::{
    names::BusName,
    zvariant::{ObjectPath, Signature},
};
use zbus_xml::{Arg, ArgDirection, Interface};

pub fn write_interfaces(
    interfaces: &[Interface<'_>],
    standard_interfaces: &[Interface<'_>],
    service: Option<BusName<'_>>,
    path: Option<ObjectPath<'_>>,
    input_src: &str,
    cargo_bin_name: &str,
    cargo_bin_version: &str,
) -> Result<String, Box<dyn Error>> {
    let mut unformatted = String::new();

    write_doc_header(
        &mut unformatted,
        interfaces,
        standard_interfaces,
        input_src,
        cargo_bin_name,
        cargo_bin_version,
    )?;

    for interface in interfaces {
        let gen = GenTrait {
            interface,
            service: service.as_ref(),
            path: path.as_ref(),
            format: false,
        };

        write!(unformatted, "{}", gen)?;
    }

    let formatted = match format_generated_code(&unformatted) {
        Ok(formatted) => formatted,
        Err(e) => {
            eprintln!("Failed to format generated code: {}", e);
            unformatted
        }
    };

    Ok(formatted)
}

/// Write a doc header, listing the included Interfaces and how the
/// code was generated.
fn write_doc_header<W: std::fmt::Write>(
    w: &mut W,
    interfaces: &[Interface<'_>],
    standard_interfaces: &[Interface<'_>],
    input_src: &str,
    cargo_bin_name: &str,
    cargo_bin_version: &str,
) -> std::fmt::Result {
    if let Some((first_iface, following_ifaces)) = interfaces.split_first() {
        if following_ifaces.is_empty() {
            writeln!(
                w,
                "//! # D-Bus interface proxy for: `{}`",
                first_iface.name()
            )?;
        } else {
            write!(
                w,
                "//! # D-Bus interface proxies for: `{}`",
                first_iface.name()
            )?;
            for iface in following_ifaces {
                write!(w, ", `{}`", iface.name())?;
            }
            writeln!(w)?;
        }
    }

    write!(
        w,
        "//!
         //! This code was generated by `{}` `{}` from D-Bus introspection data.
         //! Source: `{}`.
         //!
         //! You may prefer to adapt it, instead of using it verbatim.
         //!
         //! More information can be found in the [Writing a client proxy] section of the zbus
         //! documentation.
         //!
        ",
        cargo_bin_name, cargo_bin_version, input_src,
    )?;

    if !standard_interfaces.is_empty() {
        write!(w,
            "//! This type implements the [D-Bus standard interfaces], (`org.freedesktop.DBus.*`) for which the
             //! following zbus API can be used:
             //!
            ")?;
        for iface in standard_interfaces {
            let idx = iface.name().rfind('.').unwrap() + 1;
            let name = &iface.name()[idx..];
            writeln!(w, "//! * [`zbus::fdo::{name}Proxy`]")?;
        }
        write!(
            w,
            "//!
             //! Consequently `{}` did not generate code for the above interfaces.
            ",
            cargo_bin_name,
        )?;
    }

    write!(
        w,
        "//!
        //! [Writing a client proxy]: https://dbus2.github.io/zbus/client.html
        //! [D-Bus standard interfaces]: https://dbus.freedesktop.org/doc/dbus-specification.html#standard-interfaces,
        use zbus::proxy;
        "
    )?;

    Ok(())
}

pub struct GenTrait<'i> {
    pub interface: &'i Interface<'i>,
    pub service: Option<&'i BusName<'i>>,
    pub path: Option<&'i ObjectPath<'i>>,
    pub format: bool,
}

impl Display for GenTrait<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.format {
            let mut unformatted = String::new();
            self.write_interface(&mut unformatted)?;

            let formatted = format_generated_code(&unformatted).unwrap_or(unformatted);

            write!(f, "{}", formatted)
        } else {
            self.write_interface(f)
        }
    }
}

impl GenTrait<'_> {
    fn write_interface<W: Write>(&self, w: &mut W) -> std::fmt::Result {
        let iface = self.interface;
        let idx = iface.name().rfind('.').unwrap() + 1;
        let name = &iface.name()[idx..];

        write!(w, "#[proxy(interface = \"{}\"", iface.name())?;
        if let Some(service) = self.service {
            write!(w, ", default_service = \"{service}\"")?;
        }
        if let Some(path) = self.path {
            write!(w, ", default_path = \"{path}\"")?;
        }
        if self.path.is_none() || self.service.is_none() {
            write!(w, ", assume_defaults = true")?;
        }
        writeln!(w, ")]")?;
        writeln!(w, "pub trait {name} {{")?;

        let mut methods = iface.methods().to_vec();
        methods.sort_by(|a, b| a.name().partial_cmp(&b.name()).unwrap());
        for m in &methods {
            let (inputs, output) = inputs_output_from_args(m.args());
            let name = to_identifier(&to_snakecase(m.name().as_str()));
            writeln!(w)?;
            writeln!(w, "    /// {} method", m.name())?;
            if pascal_case(&name) != m.name().as_str() {
                writeln!(w, "    #[zbus(name = \"{}\")]", m.name())?;
            }
            hide_clippy_lints(w, m)?;
            writeln!(w, "    fn {name}({inputs}){output};")?;
        }

        let mut signals = iface.signals().to_vec();
        signals.sort_by(|a, b| a.name().partial_cmp(&b.name()).unwrap());
        for signal in &signals {
            let args = parse_signal_args(signal.args());
            let name = to_identifier(&to_snakecase(signal.name().as_str()));
            writeln!(w)?;
            writeln!(w, "    /// {} signal", signal.name())?;
            if pascal_case(&name) != signal.name().as_str() {
                writeln!(w, "    #[zbus(signal, name = \"{}\")]", signal.name())?;
            } else {
                writeln!(w, "    #[zbus(signal)]")?;
            }
            writeln!(w, "    fn {name}({args}) -> zbus::Result<()>;",)?;
        }

        let mut props = iface.properties().to_vec();
        props.sort_by(|a, b| a.name().partial_cmp(&b.name()).unwrap());
        for p in props {
            let name = to_identifier(&to_snakecase(p.name().as_str()));
            let fn_attribute = if pascal_case(&name) != p.name().as_str() {
                format!("    #[zbus(property, name = \"{}\")]", p.name())
            } else {
                "    #[zbus(property)]".to_string()
            };

            writeln!(w)?;
            writeln!(w, "    /// {} property", p.name())?;
            if p.access().read() {
                writeln!(w, "{}", fn_attribute)?;
                let output = to_rust_type(p.ty(), false, false);
                hide_clippy_type_complexity_lint(w, p.ty())?;
                writeln!(w, "    fn {name}(&self) -> zbus::Result<{output}>;",)?;
            }

            if p.access().write() {
                writeln!(w, "{}", fn_attribute)?;
                let input = to_rust_type(p.ty(), true, true);
                writeln!(
                    w,
                    "    fn set_{name}(&self, value: {input}) -> zbus::Result<()>;",
                )?;
            }
        }
        writeln!(w, "}}")
    }
}

fn hide_clippy_lints<W: Write>(write: &mut W, method: &zbus_xml::Method<'_>) -> std::fmt::Result {
    // check for <https://rust-lang.github.io/rust-clippy/master/index.html#/too_many_arguments>
    // triggers when a functions has at least 7 paramters
    if method.args().len() >= 7 {
        writeln!(write, "    #[allow(clippy::too_many_arguments)]")?;
    }

    // check for <https://rust-lang.github.io/rust-clippy/master/index.html#/type_complexity>
    for arg in method.args() {
        let signature = arg.ty();
        hide_clippy_type_complexity_lint(write, signature)?;
    }

    Ok(())
}

fn hide_clippy_type_complexity_lint<W: Write>(
    write: &mut W,
    signature: &Signature,
) -> std::fmt::Result {
    let complexity = estimate_type_complexity(signature);
    if complexity >= 1700 {
        writeln!(write, "    #[allow(clippy::type_complexity)]")?;
    }
    Ok(())
}

fn inputs_output_from_args(args: &[Arg]) -> (String, String) {
    let mut inputs = vec!["&self".to_string()];
    let mut output = vec![];
    let mut n = 0;
    let mut gen_name = || {
        n += 1;
        format!("arg_{n}")
    };

    for a in args {
        match a.direction() {
            None | Some(ArgDirection::In) => {
                let ty = to_rust_type(a.ty(), true, true);
                let arg = if let Some(name) = a.name() {
                    to_identifier(name)
                } else {
                    gen_name()
                };
                inputs.push(format!("{arg}: {ty}"));
            }
            Some(ArgDirection::Out) => {
                let ty = to_rust_type(a.ty(), false, false);
                output.push(ty);
            }
        }
    }

    let output = match output.len() {
        0 => "()".to_string(),
        1 => output[0].to_string(),
        _ => format!("({})", output.join(", ")),
    };

    (inputs.join(", "), format!(" -> zbus::Result<{output}>"))
}

fn parse_signal_args(args: &[Arg]) -> String {
    let mut inputs = vec!["&self".to_string()];
    let mut n = 0;
    let mut gen_name = || {
        n += 1;
        format!("arg_{n}")
    };

    for a in args {
        let ty = to_rust_type(a.ty(), true, false);
        let arg = if let Some(name) = a.name() {
            to_identifier(name)
        } else {
            gen_name()
        };
        inputs.push(format!("{arg}: {ty}"));
    }

    inputs.join(", ")
}

fn to_rust_type(ty: &Signature, input: bool, as_ref: bool) -> String {
    // can't haz recursive closure, yet
    fn signature_to_rust_type(signature: &Signature, input: bool, as_ref: bool) -> String {
        match signature {
            Signature::Unit => "".into(),
            Signature::U8 => "u8".into(),
            Signature::Bool => "bool".into(),
            Signature::I16 => "i16".into(),
            Signature::U16 => "u16".into(),
            Signature::I32 => "i32".into(),
            Signature::U32 => "u32".into(),
            Signature::I64 => "i64".into(),
            Signature::U64 => "u64".into(),
            Signature::F64 => "f64".into(),
            #[cfg(unix)]
            Signature::Fd if input => "zbus::zvariant::Fd<'_>".into(),
            #[cfg(unix)]
            Signature::Fd => "zbus::zvariant::OwnedFd".into(),
            Signature::Str if input || as_ref => "&str".into(),
            Signature::Str => "String".into(),
            Signature::ObjectPath if input => {
                if as_ref {
                    "&zbus::zvariant::ObjectPath<'_>".into()
                } else {
                    "zbus::zvariant::ObjectPath<'_>".into()
                }
            }
            Signature::ObjectPath => "zbus::zvariant::OwnedObjectPath".into(),
            Signature::Signature if input => {
                if as_ref {
                    "&zbus::zvariant::Signature<'_>".into()
                } else {
                    "zbus::zvariant::Signature<'_>".into()
                }
            }
            Signature::Signature => "zbus::zvariant::OwnedSignature".into(),
            Signature::Variant if input => {
                if as_ref {
                    "&zbus::zvariant::Value<'_>".into()
                } else {
                    "zbus::zvariant::Value<'_>".into()
                }
            }
            Signature::Variant => "zbus::zvariant::OwnedValue".into(),
            Signature::Array(child) => {
                let child_ty = signature_to_rust_type(child, input, as_ref);
                if input && as_ref {
                    format!("&[{}]", child_ty)
                } else {
                    format!("Vec<{}>", child_ty)
                }
            }
            Signature::Dict { key, value } => {
                let key_ty = signature_to_rust_type(key, input, as_ref);
                let value_ty = signature_to_rust_type(value, input, as_ref);

                format!("std::collections::HashMap<{}, {}>", key_ty, value_ty)
            }
            Signature::Structure(fields) => {
                let fields = fields
                    .iter()
                    .map(|f| signature_to_rust_type(f, input, as_ref))
                    .collect::<Vec<_>>();

                if fields.len() > 1 {
                    format!("{}({})", if as_ref { "&" } else { "" }, fields.join(", "))
                } else {
                    format!("{}({},)", if as_ref { "&" } else { "" }, fields[0])
                }
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("Unsupported signature: {}", signature),
        }
    }

    signature_to_rust_type(ty, input, as_ref)
}

static KWORDS: &[&str] = &[
    "Self", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue",
    "crate", "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "if", "impl",
    "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref",
    "return", "self", "static", "struct", "super", "trait", "true", "try", "type", "typeof",
    "union", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

fn to_identifier(id: &str) -> String {
    if KWORDS.contains(&id) {
        format!("{id}_")
    } else {
        id.replace('-', "_")
    }
}

// This function is the same as zbus_macros::utils::pascal_case
pub fn pascal_case(s: &str) -> String {
    let mut pascal = String::new();
    let mut capitalize = true;
    for ch in s.chars() {
        if ch == '_' {
            capitalize = true;
        } else if capitalize {
            pascal.push(ch.to_ascii_uppercase());
            capitalize = false;
        } else {
            pascal.push(ch);
        }
    }
    pascal
}

fn estimate_type_complexity(signature: &Signature) -> u32 {
    let mut score = 0;

    match signature {
        Signature::Unit => (),
        Signature::U8
        | Signature::Bool
        | Signature::I16
        | Signature::U16
        | Signature::I32
        | Signature::U32
        | Signature::I64
        | Signature::U64
        | Signature::F64
        | Signature::Str => score += 1,
        #[cfg(unix)]
        Signature::Fd => score += 10,
        Signature::ObjectPath | Signature::Signature | Signature::Variant => score += 10,
        Signature::Array(child) => score += 5 * estimate_type_complexity(child),
        Signature::Dict { key, value } => {
            score *= 10 + 50;
            score += 5 * estimate_type_complexity(key);
            score += 5 * estimate_type_complexity(value);
        }
        Signature::Structure(fields) => {
            score += 50;
            for field in fields.iter() {
                score += 5 * estimate_type_complexity(field);
            }
        }
        #[allow(unreachable_patterns)]
        _ => unreachable!("Unsupported signature: {}", signature),
    }

    score
}

fn format_generated_code(generated_code: &str) -> std::io::Result<String> {
    use std::io::{Read, Write};

    let mut process = Command::new("rustfmt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // rustfmt may post warnings about features not being enabled on stable rust
        // these can be distracting and are irrevelant to the user, so we hide them
        .stderr(Stdio::null())
        .spawn()?;
    let rustfmt_stdin = process.stdin.as_mut().unwrap();
    let mut rustfmt_stdout = process.stdout.take().unwrap();
    writeln!(rustfmt_stdin)?;
    rustfmt_stdin.write_all(generated_code.as_bytes())?;

    let exit_status = process.wait()?;
    if !exit_status.success() {
        eprintln!("`rustfmt` did not exit successfully. Continuing with unformatted code.");
        return Ok(generated_code.to_string());
    }

    let mut formatted = String::new();
    rustfmt_stdout.read_to_string(&mut formatted)?;

    Ok(formatted)
}
