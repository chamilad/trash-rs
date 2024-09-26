use std::env;
use std::error::Error;
use std::io::{stdin, stdout, Write};

use libtrash::*;

const EXITCODE_OK: i32 = 0;
const EXITCODE_INVALID_ARGS: i32 = 1;
const EXITCODE_UNSUPPORTED: i32 = 2;
const EXITCODE_EXTERNAL: i32 = 255;

// Does NOT support trashing files from external mounts to user's trash dir
// Does NOT trash a file from external mounts to home if topdirs cannot be used
fn main() {
    // skip the binary name, and parse rest of the args
    let args: Vec<String> = env::args().skip(1).collect();
    let args_conf = match parse_args(args) {
        Ok(v) => v,
        Err(e) => {
            msg_err(format!("{e}"));
            msg_err("try '-h' for more information.");
            std::process::exit(EXITCODE_INVALID_ARGS);
        }
    };

    if args_conf.version {
        let version = env!("CARGO_PKG_VERSION");
        let binary_name = env!("CARGO_PKG_NAME");
        println!("{binary_name} ({version})");
        std::process::exit(EXITCODE_OK);
    }

    if args_conf.help {
        println!("help text here todo");
        std::process::exit(EXITCODE_OK);
    }

    for file_name in args_conf.file_names {
        // get absolute path and check file exists
        let abs_file = match to_abs_path(&file_name) {
            Ok(v) => v,
            Err(_) => {
                msg_err(format!(
                    "cannot trash '{file_name}': no such file or directory"
                ));
                std::process::exit(EXITCODE_INVALID_ARGS);
            }
        };

        // When trashing a file or directory, the implementation SHOULD
        // check whether the user has the necessary permissions to delete it,
        // before starting the trashing operation itself.
        //
        // can refuse trashing because of lack of more permissions to the file
        if !can_delete_file(&abs_file) {
            msg_err(format!(
                "cannot trash '{file_name}': not enough permissions to delete the file"
            ));
            std::process::exit(EXITCODE_UNSUPPORTED);
        }

        let trash_dir = match TrashDirectory::resolve_for_file(&abs_file, args_conf.verbose) {
            Ok(v) => v,
            Err(e) => {
                msg_err(format!(
                    "cannot trash '{file_name}': cannot resolve trash directory: {e}"
                ));
                std::process::exit(EXITCODE_UNSUPPORTED);
            }
        };

        if abs_file.starts_with(&trash_dir.home) {
            msg_err(format!("trashing the trash is not supported"));
            std::process::exit(EXITCODE_UNSUPPORTED);
        }

        let mut trash_file = match TrashFile::new(abs_file) {
            Ok(v) => v,
            Err(e) => {
                msg_err(format!("cannot trash '{file_name}': {e}"));
                std::process::exit(EXITCODE_UNSUPPORTED);
            }
        };

        match trash_dir.generate_trash_entry_names(&mut trash_file) {
            Ok(_) => (),
            Err(e) => {
                msg_err(format!("cannot trash '{file_name}': {e}"));
                std::process::exit(EXITCODE_UNSUPPORTED);
            }
        }

        if args_conf.interactive {
            print!("trash file '{file_name}'? (y/n): ");
            match stdout().flush() {
                Ok(_) => (),
                Err(e) => {
                    msg_err(format!("input/output error: {e}"));
                    std::process::exit(EXITCODE_EXTERNAL);
                }
            };

            let mut confirmation = String::new();
            match stdin().read_line(&mut confirmation) {
                Ok(_) => (),
                Err(e) => {
                    msg_err(format!("input/output error: {e}"));
                    std::process::exit(EXITCODE_EXTERNAL);
                }
            };
            if confirmation.strip_suffix("\n").unwrap().to_lowercase() != "y" {
                if args_conf.verbose {
                    msg_err(format!("not trashing the file"));
                }

                std::process::exit(EXITCODE_OK);
            }
        }

        match trash_file.create_trashinfo() {
            Ok(_) => (),
            Err(e) => {
                msg_err(format!("cannot trash '{file_name}': {e}"));
                std::process::exit(EXITCODE_UNSUPPORTED);
            }
        };

        match trash_file.trash() {
            Ok(_) => (),
            Err(e) => {
                msg_err(format!("cannot trash '{file_name}': {e}"));
                std::process::exit(EXITCODE_UNSUPPORTED);
            }
        }

        if let Err(e) = trash_dir.update_dir_sizes_entry(&trash_file) {
            if args_conf.verbose {
                msg_err(format!("error while updating directorysizes: {e}"));
            }
        }
    }
}

fn parse_args(args: Vec<String>) -> Result<Args, Box<dyn Error>> {
    // need at least one arg
    if args.len() == 0 {
        return Err(Box::<dyn Error>::from("missing operand"));
    }

    let mut interactive: bool = false;
    let mut verbose: bool = false;
    let mut help: bool = false;
    let mut version: bool = false;
    let mut file_names: Vec<String> = vec![];
    let mut eoo = false; // -- is end of options
    for arg in args {
        if eoo {
            file_names.push(arg);
        } else {
            match arg.as_str() {
                "--" => eoo = true,
                "-i" | "--interactive" => interactive = true,
                "-v" | "--verbose" => verbose = true,
                "-h" | "--help" => help = true,
                "-V" | "--version" => version = true,
                "-iv" | "-vi" => {
                    verbose = true;
                    interactive = true;
                }
                _ => {
                    if arg.starts_with("-") {
                        return Err(Box::<dyn Error>::from(format!("invalid option -- '{arg}'")));
                    }

                    file_names.push(arg);
                }
            }
        }
    }

    if file_names.len() == 0 && !(help || version) {
        return Err(Box::<dyn Error>::from("missing operand"));
    }

    Ok(Args {
        interactive,
        verbose,
        help,
        version,
        file_names,
    })
}

#[derive(Debug, Clone)]
struct Args {
    interactive: bool, // -i, --interactive
    verbose: bool,     // -v, --verbose
    help: bool,        // -h, --help
    version: bool,     // -V, --version
    file_names: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_args() {
        let i: Vec<String> = vec![String::from("-iv"), String::from("somefile")];
        let args = parse_args(i);
        assert!(args.is_ok());
        let a = args.unwrap();
        assert!(a.interactive && a.verbose && !a.help && !a.version);
        assert!(a.file_names.len() == 1);

        let i: Vec<String> = vec![String::from("-vi"), String::from("somefile")];
        let args = parse_args(i);
        assert!(args.is_ok());
        let a = args.unwrap();
        assert!(a.interactive && a.verbose && !a.help && !a.version);

        let i: Vec<String> = vec![String::from("--verbose"), String::from("somefile")];
        let args = parse_args(i);
        assert!(args.is_ok());
        let a = args.unwrap();
        assert!(!a.interactive && a.verbose && !a.help && !a.version);

        let i: Vec<String> = vec![String::from("-h")];
        let args = parse_args(i);
        assert!(args.is_ok());
        let a = args.unwrap();
        assert!(!a.interactive && !a.verbose && a.help && !a.version);

        let i: Vec<String> = vec![String::from("-V")];
        let args = parse_args(i);
        assert!(args.is_ok());
        let a = args.unwrap();
        assert!(!a.interactive && !a.verbose && !a.help && a.version);

        let i: Vec<String> = vec![
            String::from("-iv"),
            String::from("--"),
            String::from("-somefile"),
        ];
        let args = parse_args(i);
        assert!(args.is_ok());
        let a = args.unwrap();
        assert!(a.interactive && a.verbose && !a.help && !a.version);
        assert!(a.file_names[0] == "-somefile");

        let i: Vec<String> = vec![
            String::from("--"),
            String::from("-iv"),
            String::from("-somefile"),
        ];
        let args = parse_args(i);
        assert!(args.is_ok());
        let a = args.unwrap();
        assert!(!a.interactive && !a.verbose && !a.help && !a.version);
        assert!(a.file_names[0] == "-iv");
        assert!(a.file_names[1] == "-somefile");

        let i: Vec<String> = vec![
            String::from("somefile"),
            String::from("--"),
            String::from("-somefile"),
        ];
        let args = parse_args(i);
        assert!(args.is_ok());
        let a = args.unwrap();
        assert!(!a.interactive && !a.verbose && !a.help && !a.version);
        assert!(a.file_names[0] == "somefile");
        assert!(a.file_names[1] == "-somefile");

        let i: Vec<String> = vec![
            String::from("-iv"),
            String::from("somefile"),
            String::from("--"),
            String::from("-somefile"),
        ];
        let args = parse_args(i);
        assert!(args.is_ok());
        let a = args.unwrap();
        assert!(a.interactive && a.verbose && !a.help && !a.version);
        assert!(a.file_names[0] == "somefile");
        assert!(a.file_names[1] == "-somefile");
    }

    #[test]
    fn test_parse_args_err() {
        let i: Vec<String> = vec![];
        let args = parse_args(i);
        assert!(args.is_err());

        // need to specify a file if not help or version
        let i: Vec<String> = vec![String::from("-v")];
        let args = parse_args(i);
        assert!(args.is_err());

        let i: Vec<String> = vec![String::from("-G")];
        let args = parse_args(i);
        assert!(args.is_err());

        // can't use help or version with other flags
        let i: Vec<String> = vec![String::from("-ivh")];
        let args = parse_args(i);
        assert!(args.is_err());
        let i: Vec<String> = vec![String::from("-ivV")];
        let args = parse_args(i);
        assert!(args.is_err());

        let i: Vec<String> = vec![String::from("--")];
        let args = parse_args(i);
        assert!(args.is_err());
    }
}
