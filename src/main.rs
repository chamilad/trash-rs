use chrono;
use std::env;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const EXITCODE_INVALID_ARGS: i32 = 1;
const EXITCODE_UNSUPPORTED: i32 = 2;
const EXITCODE_EXTERNAL_ISSUE: i32 = 3;
// const EXITCODE_UNKNOWN: i32 = 255;

// todo: could use generics for path/pathbuf places

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

    // get absolute path and check file exists
    let abs_file = match std::fs::canonicalize(file_path_arg) {
        Ok(v) => v,
        Err(e) => {
            dbg!(e);
            eprintln!("error: specified file doesn't exist or can't be accessed");
            std::process::exit(EXITCODE_INVALID_ARGS);
        }
    };

    if abs_file.is_dir() {
        eprintln!("error: directories not supported yet");
        std::process::exit(EXITCODE_UNSUPPORTED);
    }

    let trash_dir = TrashDirectory::resolve()?;
    let mut trash_file = TrashFile::new(abs_file)?;
    trash_dir.generate_trash_entry_names(&mut trash_file)?;
    trash_file.create_trashinfo()?;
    trash_file.trash()?;

    Ok(())
}

struct TrashDirectory {
    home: PathBuf,
    files: PathBuf,
    info: PathBuf,
    dir_sizes: Option<PathBuf>,
}

struct TrashFile {
    original_file: PathBuf,
    files_entry: Option<PathBuf>,
    trashinfo_entry: Option<PathBuf>,
}

impl TrashDirectory {
    // derive trash directory according to trash spec
    // todo: check if topdir exists if file is in a mounted drive
    //   file to be trashed should be considered, so is a future parameter
    // todo: map error instead of exiting here
    fn resolve() -> Result<TrashDirectory, Box<dyn Error>> {
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

        let trash_home = xdg_data_home.join("Trash");
        must_have_dir(&trash_home)?;

        println!("debug: trash dir: {}", trash_home.to_str().unwrap());

        let files_dir = trash_home.join("files");
        must_have_dir(&files_dir)?;
        let info_dir = trash_home.join("info");
        must_have_dir(&info_dir)?;

        let trash_dir = TrashDirectory {
            home: trash_home,
            files: files_dir,
            info: info_dir,
            dir_sizes: None,
        };

        Ok(trash_dir)
    }

    fn generate_trash_entry_names(&self, trash_file: &mut TrashFile) -> Result<(), Box<dyn Error>> {
        let stripped_file_name = trash_file
            .original_file
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();

        // if filename present, start testing for files with an integer suffix
        // following nautilus way of starting from 2
        // not sure what the ceiling is in nautilus, keeping 1024 for the moment
        for n in 1..1024 {
            let trashable_file_name =
                Self::get_trashable_file_name(stripped_file_name.to_string(), n);
            let file = self.files.join(trashable_file_name);
            let trashinfo = self.info.join(format!(
                "{}.trashinfo",
                file.file_name().unwrap().to_str().unwrap()
            ));
            if !file.exists() && !trashinfo.exists() {
                trash_file.files_entry = Some(file);
                trash_file.trashinfo_entry = Some(trashinfo);
                return Ok(());
            }
        }

        return Err(Box::<dyn Error>::from(
            "error: reached maximum trash file iteration",
        ));
    }

    fn get_trashable_file_name(stripped_file_name: String, idx: u32) -> String {
        // nautilus trash files when duplicated start from suffix 2
        if idx < 2 {
            return stripped_file_name;
        }

        // suffix is before the file extension if present, even if it is a dir
        // ex: test.dir.ext would be test.2.dir.ext
        if stripped_file_name.contains(".") {
            let components = stripped_file_name.splitn(2, ".").collect::<Vec<&str>>();
            return format!("{}.{}.{}", components[0], idx, components[1]);
        }

        format!("{}.{}", stripped_file_name, idx)
    }
}

impl TrashFile {
    fn new(original_file: PathBuf) -> Result<TrashFile, Box<dyn Error>> {
        if !original_file.is_absolute() {
            return Err(Box::<dyn Error>::from("file path is not absolute"));
        }

        Ok(TrashFile {
            original_file,
            files_entry: None,
            trashinfo_entry: None,
        })
    }

    fn create_trashinfo(&self) -> Result<&PathBuf, Box<dyn Error>> {
        if self.files_entry == None || self.trashinfo_entry == None {
            return Err(Box::<dyn Error>::from("trash entries are uninitialised"));
        }

        println!(
            "debug: creating trashinfo: {}",
            self.original_file.to_str().unwrap(),
        );

        let info_entry = self.trashinfo_entry.as_ref().unwrap();
        if info_entry.exists() {
            return Err(Box::<dyn Error>::from("info entry already exists"));
        }

        let now = chrono::Local::now();
        let deletion_date = now.to_rfc3339_opts(chrono::format::SecondsFormat::Secs, true);
        let trashinfo = format!(
            r#"[Trash Info]
Path={}
DeletionDate={}
"#,
            self.original_file.to_str().unwrap(),
            deletion_date
        );

        println!(
            "debug: creating trashinfo file: {}",
            info_entry.to_str().unwrap(),
        );
        let mut f = match std::fs::File::create(info_entry) {
            Ok(v) => v,
            Err(e) => {
                return Err(Box::<dyn Error>::from(format!(
                    "error while creating trashinfo entry: {}",
                    e
                )));
            }
        };

        println!(
            "debug: writing to trashinfo: {}",
            info_entry.to_str().unwrap(),
        );
        match f.write_all(trashinfo.as_bytes()) {
            Ok(_) => println!("debug: trashinfo created"),
            Err(e) => {
                return Err(Box::<dyn Error>::from(format!(
                    "error while writing to trashinfo file: {}",
                    e
                )));
            }
        };

        Ok(info_entry)
    }

    fn trash(&self) -> Result<&PathBuf, Box<dyn Error>> {
        if self.files_entry == None || self.trashinfo_entry == None {
            return Err(Box::<dyn Error>::from("trash entries are uninitialised"));
        }

        let files_entry = self.files_entry.as_ref().unwrap();
        println!(
            "debug: moving {} to {}",
            self.original_file.to_str().unwrap(),
            files_entry.to_str().unwrap()
        );
        fs::rename(&self.original_file, files_entry)?;
        Ok(files_entry)
    }
}

// retrieve os defined home directory. $HOME MUST be defined as of now.
// todo: lookup passwd for home dir entry if $HOME isn't defined
fn get_home_dir() -> Result<PathBuf, Box<dyn Error>> {
    let home_dir = env::var("HOME")?;
    let home_path = Path::new(&home_dir);

    Ok(home_path.to_path_buf())
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
