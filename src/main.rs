use chrono;
use std::env;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const EXITCODE_INVALID_ARGS: i32 = 1;
const EXITCODE_UNSUPPORTED: i32 = 2;
const EXITCODE_EXTERNAL_ISSUE: i32 = 3;
const EXITCODE_UNKNOWN: i32 = 255;

// todo: could use generics for path/pathbuf places

struct TrashFile {
    original_file: PathBuf,
    files_entry: PathBuf,
    trashinfo_entry: PathBuf,
}

fn main() -> Result<(), Box<dyn Error>> {
    // parse the args
    let args: Vec<String> = env::args().collect();

    // if there's just one arg, that should be the filename, not a flag
    if args.len() < 2 {
        eprintln!("error: missing file name");
        std::process::exit(EXITCODE_INVALID_ARGS);
    }

    if args[args.len() - 1].starts_with("-") {
        eprintln!("error: missing file name");
        std::process::exit(EXITCODE_INVALID_ARGS);
    }

    // let flags: Vec<String> = Vec::new();
    // todo: last item is file for now
    let file_path_arg: &String = &args[args.len() - 1];

    // todo: clean args
    // check all but last arguments are flags
    // for arg in &args[1..args.len()-2] {

    // }

    // 1. check if file/dir exists
    //
    // dbg!(args);

    // check if file exists
    let abs_file = std::fs::canonicalize(file_path_arg)?;
    // let file_path = Path::new(&file_path_arg);
    // if !file_path.is_absolute() {
    // file_path = Path
    // }

    match abs_file.try_exists() {
        Ok(true) => {
            if abs_file.is_dir() {
                eprintln!("error: directories not supported yet");
                std::process::exit(EXITCODE_UNSUPPORTED);
            }
        }
        Ok(false) => {
            eprintln!("error: specified file doesn't exist");
            std::process::exit(EXITCODE_INVALID_ARGS);
        }
        Err(e) => {
            dbg!(e);
            eprintln!("error: error while checking file");
            std::process::exit(EXITCODE_UNKNOWN);
        }
    }

    // get the trash dir
    let trash_home = get_trash_dir(&abs_file)?;

    // check if file with same name exists
    let stripped_file_name = abs_file.file_name().unwrap().to_str().unwrap();
    let trash_file = get_trash_file_name(&trash_home, stripped_file_name)?;

    // let trash_file_name = format!(
    //     "{}/files/{}{}",
    //     trash_path.to_str().unwrap(),
    //     stripped_file_name,
    //     trash_file_suffix
    // );
    // let trash_file = Path::new(&trash_file_name);

    // add an .trashinfo entry
    create_info(
        &trash_home,
        &abs_file.to_str().unwrap(),
        &trash_file.file_name().unwrap().to_str().unwrap(),
    )?;

    // move the file
    // let file_abs = std::fs::canonicalize(file_path)?;
    move_to_trash(&abs_file, &trash_file)?;

    Ok(())
}

// derive trash directory according to trash spec
// todo: check if topdir exists if file is in a mounted drive
//   file to be trashed should be considered, so is a future parameter
fn get_trash_dir(_: &Path) -> Result<PathBuf, Box<dyn Error>> {
    // if XDG_DATA_HOME is not defined, fallback to $HOME/.local/share
    let xdg_data_home = match env::var("XDG_DATA_HOME") {
        Ok(v) => Path::new(&v).to_path_buf(),
        Err(_) => {
            let home_dir = match get_home_dir() {
                Ok(v) => v,
                Err(_) => {
                    eprintln!("error: couldn't retrieve home directory location");
                    std::process::exit(EXITCODE_EXTERNAL_ISSUE);
                }
            };

            home_dir.join(".local").join("share")
        }
    };

    let trash_dir = xdg_data_home.join("Trash");
    must_have_dir(&trash_dir)?;

    println!("debug: trash dir: {}", trash_dir.to_str().unwrap());
    Ok(trash_dir)
}

// retrieve os defined home directory. $HOME MUST be defined as of now.
// todo: lookup passwd for home dir entry if $HOME isn't defined
fn get_home_dir() -> Result<PathBuf, Box<dyn Error>> {
    let home_dir = env::var("HOME")?;
    let home_path = Path::new(&home_dir);

    Ok(home_path.to_path_buf())
}

// derive the suffix the trash file should have if duplicate files already exist in the trash
// todo: consider if a file isn't in files dir, but an entry is in info dir, or file or dir by the same name exists
fn get_trash_file_name(trash_home: &PathBuf, file_name: &str) -> Result<PathBuf, Box<dyn Error>> {
    let files_dir = trash_home.join("files");
    must_have_dir(&files_dir)?;

    let info_dir = trash_home.join("info");
    must_have_dir(&info_dir)?;

    {
        let trash_file = files_dir.join(file_name);
        let info_file = info_dir.join(file_name);
        // let trash_file_name = format!("{}/{}", trash_path, file_name);
        // let trash_file = Path::new(&trash_file_name);
        if !trash_file.exists() && !info_file.exists() {
            return Ok(trash_file);
        }
    }

    for n in 2..1024 {
        // let trash_file_name = format!("{}/{}.{}", trash_path, file_name, n);
        let trash_file = files_dir.join(format!("{}.{}", file_name, n));
        let info_file = info_dir.join(format!("{}.{}.trashinfo", file_name, n));
        // let trash_file_d = Path::new(&trash_file_name);
        if !trash_file.exists() {
            return Ok(trash_file);
        }
    }

    return Err(Box::<dyn Error>::from(
        "error: reached maximum trash file iteration",
    ));
}

// create trashinfo entry
fn create_info(
    trash_home: &PathBuf,
    orig_file_path: &str,
    trash_file_name: &str,
) -> Result<(), Box<dyn Error>> {
    println!("debug: creating trashinfo: {}", trash_file_name,);

    let info_file = info_dir.join(format!("{}.trashinfo", trash_file_name));
    if info_file.exists() {
        return Err(Box::<dyn Error>::from("info entry already exists"));
    }

    let now = chrono::Local::now();
    let deletion_date = now.to_rfc3339_opts(chrono::format::SecondsFormat::Secs, true);
    let trashinfo = format!(
        r#"[Trash Info]
Path={}
DeletionDate={}
"#,
        orig_file_path, deletion_date
    );

    println!("debug: creating trashinfo file: {}", trash_file_name,);
    let mut f = match std::fs::File::create(info_file) {
        Ok(v) => v,
        Err(e) => {
            return Err(Box::<dyn Error>::from(format!(
                "error while creating trashinfo entry: {}",
                e
            )));
        }
    };

    println!("debug: writing to trashinfo: {}", trash_file_name,);
    match f.write_all(trashinfo.as_bytes()) {
        Ok(_) => println!("debug: trashinfo created"),
        Err(e) => {
            return Err(Box::<dyn Error>::from(format!(
                "error while writing to trashinfo file: {}",
                e
            )));
        }
    };

    Ok(())
}

fn move_to_trash(orig_file: &Path, trash_file: &Path) -> Result<(), Box<dyn Error>> {
    println!(
        "debug: moving {} to {}",
        orig_file.to_str().unwrap(),
        trash_file.to_str().unwrap()
    );
    fs::rename(orig_file, trash_file)?;
    Ok(())
}

// make sure the specified path exists as a directory.
// if the path doesn't exist, the directory is created.
// if it exists and is not a directory, an Error is returned
fn must_have_dir(path: &PathBuf) -> Result<(), Box<dyn Error>> {
    match path.try_exists() {
        Ok(true) => {
            if !path.is_dir() {
                return Err(Box::<dyn Error>::from(format!(
                    "path exists but is not a directory: {}",
                    path.to_str().unwrap()
                )));
            }
        }
        Ok(false) => {
            return fs::create_dir(path).map_err(|e| {
                Box::<dyn Error>::from(format!(
                    "error: cannot create directory: {}, {}",
                    path.to_str().unwrap(),
                    e,
                ))
            });
        }
        Err(_) => {
            return Err(Box::<dyn Error>::from(format!(
                "error: cannot verify directory exists: {}",
                path.to_str().unwrap()
            )));
        }
    };

    Ok(())
}
