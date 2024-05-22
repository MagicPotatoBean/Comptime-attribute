extern crate proc_macro;
use std::{
    collections::{hash_map::Entry, HashMap},
    hash::{DefaultHasher, Hash, Hasher},
    io::Write,
    path::Path,
    process::Command,
    time::Instant,
};

use proc_macro::TokenStream;
use quote::{quote, ToTokens};
use syn::{parse_macro_input, Block, ItemFn};

pub fn comptime_impl(_args: TokenStream, input: TokenStream) -> TokenStream {
    // Parse the input as `ItemFn` which is a type provided
    // by `syn` to represent a function.
    let input = parse_macro_input!(input as ItemFn);

    let ItemFn {
        // The function signature
        sig,
        // The visibility specifier of this function
        vis,
        // The function block or body
        block,
        // Other attributes applied to this function
        attrs,
    } = input;

    let mut hasher = DefaultHasher::new();
    Instant::now().hash(&mut hasher);
    block.to_token_stream().to_string().hash(&mut hasher);
    let disambiguator = hasher.finish();

    let comptime_rs = format!("comptime-{}.rs", disambiguator);

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&comptime_rs)
        .expect("Failed to create comptime.rs")
        .write_all(
            format!(
                "fn main() {{ let result = {{{}}}; print!(\"{{}}\", quote::quote!(#result))   }}",
                block.to_token_stream().to_string()
            )
            .as_bytes(),
        )
        .expect("Failed to write to comptime.rs");

    Command::new("rustfmt").arg(&comptime_rs).output().ok();
    let args: Vec<_> = std::env::args().collect();
    let get_arg = |arg| {
        args.iter()
            .position(|a| a == arg)
            .and_then(|p| args.get(p + 1))
    };

    let out_dir = match get_arg("--out-dir") {
        Some(out_dir) => Path::new(out_dir),
        None => {
            panic!("comptime failed: could not determine rustc out dir.");
        }
    };

    let mut rustc_args = filter_rustc_args(&args);
    rustc_args.push("--crate-name".to_string());
    rustc_args.push("comptime_bin".to_string());
    rustc_args.push("--crate-type".to_string());
    rustc_args.push("bin".to_string());
    rustc_args.push("--emit=dep-info,link".to_string());
    rustc_args.append(&mut merge_externs(&out_dir, &args));
    rustc_args.push(comptime_rs.clone());

    let compile_output = Command::new("rustc")
        .args(&rustc_args)
        .output()
        .expect("could not invoke rustc");
    if !compile_output.status.success() {
        panic!(
            "could not compile comptime expr:\n\n{}\n",
            String::from_utf8(compile_output.stderr).unwrap()
        );
    }

    let extra_filename = args
        .iter()
        .find(|a| a.starts_with("extra-filename="))
        .map(|ef| ef.split('=').nth(1).unwrap())
        .unwrap_or_default();
    let comptime_bin = out_dir.join(format!("comptime_bin{}", extra_filename));

    let comptime_output = Command::new(&comptime_bin)
        .output()
        .expect("could not invoke comptime_bin");

    if !comptime_output.status.success() {
        panic!(
            "could not run comptime expr:\n\n{}\n",
            String::from_utf8(comptime_output.stderr).unwrap()
        );
    }

    let comptime_expr_str = match String::from_utf8(comptime_output.stdout) {
        Ok(output) => output,
        Err(_) => panic!("comptime expr output was not utf8"),
    };

    let comptime_expr: syn::Expr = match syn::parse_str(&comptime_expr_str) {
        Ok(expr) => expr,
        Err(_) => syn::ExprLit {
            attrs: Vec::new(),
            lit: syn::LitStr::new(&comptime_expr_str, proc_macro2::Span::call_site()).into(),
        }
        .into(),
    };

    std::fs::remove_file(comptime_rs).ok();
    std::fs::remove_file(comptime_bin).ok();

    let result = comptime_expr.to_token_stream();
    // Reconstruct the function as output using parsed input
    quote!(
        #(#attrs)*
        #vis #sig {
            #result
        }
    )
    .into()
}

/// Line-for-line copy of the (comptime)[https://docs.rs/comptime/latest/comptime/] crate
/// Returns the rustc args needed to build the comptime executable.
fn filter_rustc_args(args: &[String]) -> Vec<String> {
    let mut rustc_args = Vec::with_capacity(args.len());
    let mut skip = true; // skip the invoked program
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg == "--crate-type" || arg == "--crate-name" || arg == "--extern" {
            skip = true;
        } else if arg.ends_with(".rs")
            || arg == "--test"
            || arg == "rustc"
            || arg.starts_with("--emit")
        {
            continue;
        } else {
            rustc_args.push(arg.clone());
        }
    }
    rustc_args
}

/// Line-for-line copy of the (comptime)[https://docs.rs/comptime/latest/comptime/] crate
fn merge_externs(deps_dir: &Path, args: &[String]) -> Vec<String> {
    let mut cargo_rlibs = HashMap::new(); // libfoo -> /path/to/libfoo-12345.rlib
    let mut next_is_extern = false;
    for arg in args {
        if next_is_extern {
            let mut libname_path = arg.split('=');
            let lib_name = libname_path.next().unwrap(); // libfoo
            let path = Path::new(libname_path.next().unwrap());
            if path.extension().unwrap() == "rlib" {
                cargo_rlibs.insert(lib_name.to_string(), path.to_path_buf());
            }
        }
        next_is_extern = arg == "--extern";
    }

    let mut dep_dirents: Vec<_> = std::fs::read_dir(deps_dir)
        .unwrap()
        .filter_map(|de| {
            let de = de.unwrap();
            let p = de.path();
            let fname = p.file_name().unwrap().to_str().unwrap();
            if fname.starts_with("lib") && fname.ends_with(".rlib") {
                Some(de)
            } else {
                None
            }
        })
        .collect();
    dep_dirents.sort_by_key(|de| std::cmp::Reverse(de.metadata().and_then(|m| m.created()).ok()));

    for dirent in dep_dirents {
        let path = dirent.path();
        let fname = path.file_name().unwrap().to_str().unwrap();
        if !fname.ends_with(".rlib") {
            continue;
        }
        let lib_name = fname.rsplitn(2, '-').nth(1).unwrap().to_string();
        // ^ reverse "libfoo-disambiguator" then split off the disambiguator
        if let Entry::Vacant(ve) = cargo_rlibs.entry(lib_name) {
            ve.insert(path);
        }
    }

    let mut merged_externs = Vec::with_capacity(cargo_rlibs.len() * 2);
    for (lib_name, path) in cargo_rlibs.iter() {
        merged_externs.push("--extern".to_string());
        merged_externs.push(format!("{}={}", &lib_name.strip_prefix("lib").unwrap_or(lib_name), path.display()));
    }

    merged_externs
}
