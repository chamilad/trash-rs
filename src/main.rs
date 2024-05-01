use chrono;
use std::convert::TryInto;
use std::env;
use std::error::Error;
use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::os::linux::fs::MetadataExt;
use std::path::{Path, PathBuf};

use libc;

const EXITCODE_INVALID_ARGS: i32 = 1;
const EXITCODE_UNSUPPORTED: i32 = 2;
// const EXITCODE_EXTERNAL_ISSUE: i32 = 3;
// const EXITCODE_UNKNOWN: i32 = 255;
//

const BINARY_NAME: &str = "trash-rs";

// Does NOT support trashing files from external mounts to user's trash dir
// Does NOT trash a file from external mounts to home if topdirs cannot be used

// todo: could use generics for path/pathbuf places

fn main() {
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
        Err(_) => {
            // dbg!(e);
            eprintln!("{BINARY_NAME}: cannot trash '{file_path_arg}': no such file or directory");
            std::process::exit(EXITCODE_INVALID_ARGS);
        }
    };

    let trash_dir = match TrashDirectory::resolve_for_file(&abs_file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{BINARY_NAME}: cannot trash '{file_path_arg}': cannot resolve trash directory: {e}");
            std::process::exit(EXITCODE_UNSUPPORTED);
        }
    };

    if abs_file.starts_with(&trash_dir.home) {
        eprintln!("{BINARY_NAME}: trashing the trash is not supported");
        std::process::exit(EXITCODE_UNSUPPORTED);
    }

    let mut trash_file = match TrashFile::new(abs_file) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{BINARY_NAME}: cannot trash '{file_path_arg}': {e}");
            std::process::exit(EXITCODE_UNSUPPORTED);
        }
    };

    match trash_dir.generate_trash_entry_names(&mut trash_file) {
        Ok(_) => (),
        Err(e) => {
            eprintln!("{BINARY_NAME}: cannot trash '{file_path_arg}': {e}");
            std::process::exit(EXITCODE_UNSUPPORTED);
        }
    }

    match trash_file.create_trashinfo(&trash_dir) {
        Ok(_) => (),
        Err(e) => {
            eprintln!("{BINARY_NAME}: cannot trash '{file_path_arg}': {e}");
            std::process::exit(EXITCODE_UNSUPPORTED);
        }
    };

    match trash_file.trash() {
        Ok(_) => (),
        Err(e) => {
            eprintln!("{BINARY_NAME}: cannot trash '{file_path_arg}': {e}");
            std::process::exit(EXITCODE_UNSUPPORTED);
        }
    }
}

// #[derive(Debug)]
// struct ErrorTopDirUnusable {
//     msg: String,
// }

// impl fmt::Display for ErrorTopDirUnusable {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//         write!(
//             f,
//             "an error occurred while trying to derive top directory for file"
//         )
//     }
// }

enum TrashRootType {
    Home,
    TopDirAdmin,
    TopDirUser,
}

struct TrashDirectory {
    device: Device,
    home: PathBuf,
    files: PathBuf,
    info: PathBuf,
    dir_sizes: Option<PathBuf>,
    root_type: TrashRootType,
}

struct TrashFile {
    original_file: PathBuf,
    files_entry: Option<PathBuf>,
    trashinfo_entry: Option<PathBuf>,
}

impl TrashDirectory {
    // derive trash directory according to trash spec
    // todo: support expunge dir (not sure how to schedule job for permanent deletion)
    fn resolve_for_file(abs_file_path: &PathBuf) -> Result<TrashDirectory, Box<dyn Error>> {
        // check if the file is in a home mount
        // "To be more precise, from a partition/device different from the one on which $XDG_DATA_HOME resides"
        let xdg_data_home = get_xdg_data_home()?;
        let mut file_dev = Device::for_path(abs_file_path)?;
        let xdg_data_home_dev = Device::for_path(&xdg_data_home)?;
        let mut trash_root_type: TrashRootType;

        let trash_home = if file_dev.dev_num.dev_id == xdg_data_home_dev.dev_num.dev_id {
            println!(
                "file is in home mount: {}, {}",
                file_dev.dev_num.dev_id, xdg_data_home_dev.dev_num.dev_id
            );

            // For every user a “home trash” directory MUST be available. Its
            // name and location are $XDG_DATA_HOME/Trash
            // If this directory is needed for a trashing operation but does
            // not exist, the implementation SHOULD automatically create it,
            // without any warnings or delays
            let trash_home = xdg_data_home.join("Trash");
            must_have_dir(&trash_home)?;
            trash_root_type = TrashRootType::Home;

            trash_home
        } else {
            println!(
                "file is in external mount: {}, {}",
                file_dev.dev_num.dev_id, xdg_data_home_dev.dev_num.dev_id
            );
            file_dev.resolve_mount()?;
            let top_dir = file_dev.mount_point.clone().unwrap();

            // user specific directory name
            // todo: int test with a another user
            let euid: u32;
            unsafe {
                euid = libc::geteuid();
            }

            let trash_home = match Self::try_topdir_admin_trash(top_dir.clone(), euid) {
                Ok(p) => {
                    trash_root_type = TrashRootType::TopDirAdmin;
                    p
                }
                Err(_) => {
                    // if the method (1) fails at any point — that is, the $topdir/.
                    // Trash directory does not exist, or it fails the checks, or the
                    // system refuses to create an $uid directory in it — the
                    // implementation MUST, by default, fall back to method (2)
                    //
                    let p = Self::try_topdir_user_trash(top_dir, euid)?;
                    trash_root_type = TrashRootType::TopDirUser;
                    p
                }
            };

            trash_home

            // if admin_trash_available {
            // } else {
            // }
        };

        let files_dir = trash_home.join("files");
        must_have_dir(&files_dir)?;

        let info_dir = trash_home.join("info");
        must_have_dir(&info_dir)?;

        println!("debug: trash dir: {}", trash_home.to_str().unwrap());
        let trash_dir = TrashDirectory {
            device: file_dev,
            root_type: trash_root_type,
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
        // not sure what the ceiling is in nautilus
        // looks like there's no real limit in giolocalfile
        // https://gitlab.gnome.org/GNOME/glib/-/blob/main/gio/glocalfile.c?ref_type=heads#L2234
        for n in 1..u32::MAX {
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
            "error: reached maximum trash file name iteration",
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

    fn try_topdir_admin_trash(
        top_dir: PathBuf,
        euid: libc::uid_t,
    ) -> Result<PathBuf, Box<dyn Error>> {
        // An administrator can create an $topdir/.Trash directory. The
        // permissions on this directories should permit all users who
        // can trash files at all to write in it.; and the “sticky bit”
        // in the permissions must be set, if the file system supports it.
        //
        // check if $topdir/.Trash exist
        // todo: check if writable
        let admin_trash = top_dir.join(".Trash");
        match admin_trash.try_exists() {
            Ok(true) => {
                // If this directory is present, the implementation MUST,
                // by default, check for the “sticky bit”.
                // todo: provide superusers to disable this check to
                // support filesystems that don't support sticky bit.
                //
                // The implementation also MUST check that this directory
                // is not a symbolic link.
                //

                // test if user can write to this dir
                let writable: libc::c_int;
                let path_cstr = CString::new(admin_trash.file_name().unwrap().to_str().unwrap())?;
                unsafe {
                    writable = libc::access(path_cstr.as_ptr(), libc::W_OK);
                }

                // access manpage for ubuntu: On success (all requested
                // permissions granted, or mode is F_OK and the file exists),
                // zero is returned.
                if writable != 0 {
                    return Err(Box::<dyn Error>::from("top directory trash isn't writable"));
                }

                // check if sticky bit is set and is not a symlink
                let mode = admin_trash.metadata()?.st_mode();
                // println!("mode: {:#034b}, {:#X}, {}", mode, mode, mode);
                let sticky_bit_set = mode & libc::S_ISVTX == libc::S_ISVTX;
                if sticky_bit_set && !admin_trash.is_symlink() {
                    // topdir approach 1
                    //
                    //  if this directory does not exist for the current user, the
                    //  implementation MUST immediately create it, without any
                    //  warnings or delays for the user.
                    //
                    // $topdir/.Trash/$uid
                    let user_trash_home = admin_trash.join(euid.to_string());
                    must_have_dir(&user_trash_home)?;

                    Ok(user_trash_home)
                } else {
                    Err(Box::<dyn Error>::from(
                        "top directory trash is a symlink or sticky bit not set",
                    ))
                }

                // todo: Besides, the implementation SHOULD report the
                // failed check to the administrator, and MAY also report it to the user.
            }
            _ => Err(Box::<dyn Error>::from("top directory trash does not exist")),
        }
    }

    fn try_topdir_user_trash(
        top_dir: PathBuf,
        euid: libc::uid_t,
    ) -> Result<PathBuf, Box<dyn Error>> {
        // topdir approach 2
        //
        // todo: The implementation MAY, however, provide a way for the
        // administrator to disable (2) completely.
        //
        // the implementation MUST immediately create it
        //
        // $topdir/.Trash-uid
        let user_trash_name = format!(".Trash-{}", euid);
        let user_trash_home = top_dir.join(user_trash_name);
        must_have_dir(&user_trash_home)?;

        Ok(user_trash_home)
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

    fn create_trashinfo(&self, trash_dir: &TrashDirectory) -> Result<&PathBuf, Box<dyn Error>> {
        if self.files_entry == None || self.trashinfo_entry == None {
            return Err(Box::<dyn Error>::from("trash entries are uninitialised"));
        }

        println!(
            "debug: creating trashinfo: {}",
            self.original_file.to_str().unwrap(),
        );

        let relative_path: PathBuf;
        // The system SHOULD support absolute pathnames only in the “home trash” directory, not in the directories under $topdir
        let file_path_key = match trash_dir.root_type {
            TrashRootType::Home => self.original_file.to_str().unwrap(),
            _ => {
                let trash_home_mt_point = trash_dir.device.mount_point.as_ref().unwrap();
                relative_path = get_path_relative_to(&self.original_file, &trash_home_mt_point)?;
                relative_path.to_str().unwrap()
            }
        };

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
            file_path_key, deletion_date
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

// retrieve XDG_DATA_HOME value, from env var or falling back to spec default
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

// returns a PathBuf of a relative path of child against parent
fn get_path_relative_to(child: &PathBuf, parent: &PathBuf) -> Result<PathBuf, Box<dyn Error>> {
    if !child.is_absolute() || !parent.is_absolute() {
        return Err(Box::<dyn Error>::from("require absolute paths"));
    }

    let stripped = child.strip_prefix(parent)?;
    Ok(stripped.to_path_buf())
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

struct DeviceNumber {
    dev_id: u64,
    major: u32,
    minor: u32,
}

impl DeviceNumber {
    // latest device drivers ref - Ch3
    // Within the kernel, the dev_t type (defined in <linux/types.h>) is used to hold device
    // numbers—both the major and minor parts. As of Version 2.6.0 of the kernel, dev_t is
    // a 32-bit quantity with 12 bits set aside for the major number and 20 for the minor
    // number. Your code should, of course, never make any assumptions about the inter-
    // nal organization of device numbers;
    fn for_path(abs_file_path: &PathBuf) -> Result<DeviceNumber, Box<dyn Error>> {
        let f_metadata = abs_file_path.metadata()?;
        let file_device_id = f_metadata.st_dev();

        let major: u32;
        let minor: u32;

        unsafe {
            major = libc::major(file_device_id);
            minor = libc::minor(file_device_id);
        }

        let dev_number = DeviceNumber {
            dev_id: file_device_id,
            major: major.try_into().unwrap(),
            minor: minor.try_into().unwrap(),
        };

        Ok(dev_number)
    }
}
