use chrono;
use std::convert::TryInto;
use std::env;
use std::error::Error;
use std::fs;
use std::io::Write;
use std::os::linux::fs::MetadataExt;
use std::path::{Path, PathBuf};

use libc;

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

    // todo: block trashing the trash

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

    let trash_dir = TrashDirectory::resolve_for_file(&abs_file)?;
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
    // todo: support expunge dir (not sure how to schedule job for permanent deletion)
    fn resolve_for_file(abs_file_path: &PathBuf) -> Result<TrashDirectory, Box<dyn Error>> {
        // check if the file is in a home mount
        let xdg_data_home = get_xdg_data_home()?;
        let mut file_dev = Device::for_path(abs_file_path)?;
        // todo: not home dir, it's xdg_data_home to be precise
        let home_dev = Device::for_path(&xdg_data_home)?;

        let trash_home = if file_dev.dev_num.dev_id == home_dev.dev_num.dev_id {
            println!(
                "file is in home mount: {}, {}",
                file_dev.dev_num.dev_id, home_dev.dev_num.dev_id
            );

            // For every user a “home trash” directory MUST be available. Its
            // name and location are $XDG_DATA_HOME/Trash
            // If this directory is needed for a trashing operation but does
            // not exist, the implementation SHOULD automatically create it,
            // without any warnings or delays
            let trash_home = xdg_data_home.join("Trash");
            must_have_dir(&trash_home)?;
            trash_home
        } else {
            // todo: path in trashinfo should be relative, SHOULD not support absolute path names in external mounts
            println!(
                "file is in external mount: {}, {}",
                file_dev.dev_num.dev_id, home_dev.dev_num.dev_id
            );
            file_dev.resolve_mount()?;
            let top_dir = file_dev.mount_point.unwrap();

            // check if $topdir/.Trash exist
            let admin_trash = top_dir.join(".Trash");
            let admin_trash_available: bool = match admin_trash.try_exists() {
                Ok(true) => {
                    // check if sticky bit is set and is not a symlink
                    let mode = admin_trash.metadata()?.st_mode();
                    println!("mode: {:#034b}, {:#X}, {}", mode, mode, mode);
                    mode & libc::S_ISVTX == libc::S_ISVTX && !admin_trash.is_symlink()
                }
                _ => false,
            };

            if admin_trash_available {
                // $topdir/.Trash/$uid
                let euid: u32;
                unsafe {
                    euid = libc::geteuid();
                    println!("euid: {}", euid);
                }
                let user_trash_home = admin_trash.join(euid.to_string());
                must_have_dir(&user_trash_home)?;
                user_trash_home
            } else {
                // $topdir/.Trash-uid
                let user_trash_name: String;
                unsafe {
                    let euid = libc::geteuid();
                    println!("euid: {}", euid);
                    user_trash_name = format!(".Trash-{}", euid);
                }

                let user_trash_home = top_dir.join(user_trash_name);
                must_have_dir(&user_trash_home)?;
                user_trash_home
            }
        };

        let files_dir = trash_home.join("files");
        must_have_dir(&files_dir)?;

        let info_dir = trash_home.join("info");
        must_have_dir(&info_dir)?;

        println!("debug: trash dir: {}", trash_home.to_str().unwrap());
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

// todo
fn get_xdg_data_home() -> Result<PathBuf, Box<dyn Error>> {
    // if XDG_DATA_HOME is not defined, fallback to $HOME/.local/share
    let xdg_data_home = match env::var("XDG_DATA_HOME") {
        Ok(v) => Path::new(&v).to_path_buf(),
        Err(_) => {
            let home_dir = get_home_dir().map_err(|_| {
                Box::<dyn Error>::from("error: couldn't retrieve home directory location")
            });

            home_dir?.join(".local").join("share")
        }
    };

    Ok(xdg_data_home)
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

struct Device {
    dev_num: DeviceNumber,
    dev_name: Option<String>,
    mount_root: Option<PathBuf>,
    mount_point: Option<PathBuf>,
}

impl Device {
    // man 5 proc
    const PROCINFO_FIELD_MAJORMINOR: usize = 2;
    const PROCINFO_FIELD_MOUNT_ROOT: usize = 3;
    const PROCINFO_FIELD_MOUNT_POINT: usize = 4;
    const PROCINFO_FIELD_DEV_NAME: usize = 9;

    fn for_path(abs_file_path: &PathBuf) -> Result<Device, Box<dyn Error>> {
        let dev_id = DeviceNumber::for_path(abs_file_path)?;
        Ok(Device {
            dev_num: dev_id,
            dev_name: None,
            mount_root: None,
            mount_point: None,
        })
    }

    fn resolve_mount(&mut self) -> Result<(), Box<dyn Error>> {
        let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap();
        let mounts: Vec<&str> = mountinfo.lines().collect();
        for mount in mounts {
            let fields: Vec<&str> = mount.split_whitespace().collect();
            if fields[Self::PROCINFO_FIELD_MAJORMINOR]
                == format!("{}:{}", self.dev_num.major, self.dev_num.minor)
            {
                self.dev_name = Some(fields[Self::PROCINFO_FIELD_DEV_NAME].to_string());
                self.mount_root = Some(PathBuf::from(
                    fields[Self::PROCINFO_FIELD_MOUNT_ROOT].to_string(),
                ));
                self.mount_point = Some(PathBuf::from(
                    fields[Self::PROCINFO_FIELD_MOUNT_POINT].to_string(),
                ));

                return Ok(());
            }
        }

        Err(Box::<dyn Error>::from(
            "could not find mount point for dev id",
        ))
    }
}

// both Major and Minor numbers are 8 bit ints - Linux Device Drivers, 2nd Edition
struct DeviceNumber {
    dev_id: u16,
    major: u8,
    minor: u8,
}

impl DeviceNumber {
    // todo: might be different after kernel v2.16, need to check with latest driver docs
    const MASK_MAJOR: u16 = 0xFF00;
    const MASK_MINOR: u16 = 0xFF;

    fn for_path(abs_file_path: &PathBuf) -> Result<DeviceNumber, Box<dyn Error>> {
        let f_metadata = abs_file_path.metadata()?;
        let file_device_id: u16 = f_metadata.st_dev().try_into().unwrap();
        println!("device_id: {:#010b}, {:#X}", file_device_id, file_device_id);

        let mut major = file_device_id & Self::MASK_MAJOR;
        major = major >> 8;
        let minor = file_device_id & Self::MASK_MINOR;

        println!("major: {:#010b}, {:#X}, {}", major, major, major);
        println!("minor: {:#010b}, {:#X}, {}", minor, minor, minor);

        let dev_number = DeviceNumber {
            dev_id: file_device_id,
            major: major.try_into().unwrap(),
            minor: minor.try_into().unwrap(),
        };

        Ok(dev_number)
    }
}
