use chrono;
use lazy_static::lazy_static;
use std::convert::TryInto;
use std::env;
use std::error::Error;
use std::ffi::CString;
use std::fs;
use std::io::{stdin, stdout, Write};
use std::os::linux::fs::MetadataExt;
use std::path::PathBuf;

use libc;

const EXITCODE_OK: i32 = 0;
const EXITCODE_INVALID_ARGS: i32 = 1;
const EXITCODE_UNSUPPORTED: i32 = 2;
const EXITCODE_EXTERNAL: i32 = 255;

// Does NOT support trashing files from external mounts to user's trash dir
// Does NOT trash a file from external mounts to home if topdirs cannot be used

lazy_static! {
    pub static ref BINARY_NAME: String = match env::var("CARGO_PKG_NAME") {
        Ok(v) => v,
        Err(_) => "trash-rs-default".to_string(),
    };
}

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
        let version = match env::var("CARGO_PKG_VERSION") {
            Ok(v) => v,
            Err(_) => "latest".to_string(),
        };
        let binary_name = &*BINARY_NAME;
        println!("{binary_name} ({version})");
        std::process::exit(EXITCODE_OK);
    }

    if args_conf.help {
        println!("help text here todo");
        std::process::exit(EXITCODE_OK);
    }

    for file_name in args_conf.file_names {
        // get absolute path and check file exists
        let abs_file = match std::fs::canonicalize(&file_name) {
            Ok(v) => v,
            Err(_) => {
                msg_err(format!(
                    "cannot trash '{file_name}': no such file or directory"
                ));
                std::process::exit(EXITCODE_INVALID_ARGS);
            }
        };

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

        match trash_file.create_trashinfo(&trash_dir) {
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
    }
}

// todo: support merged flags ex: -iv
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
    for arg in args {
        match arg.as_str() {
            "-i" | "--interactive" => interactive = true,
            "-v" | "--verbose" => verbose = true,
            "-h" | "--help" => help = true,
            "-V" | "--version" => version = true,
            _ => {
                if arg.starts_with("-") {
                    return Err(Box::<dyn Error>::from(format!("invalid option -- '{arg}'")));
                }

                file_names.push(arg);
            }
        }
    }

    if file_names.len() == 0 && (interactive || verbose) {
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

// todo: test for files that start with -, ex: -foo
#[derive(Debug, Clone)]
struct Args {
    interactive: bool, // -i, --interactive
    verbose: bool,     // -v, --verbose
    help: bool,        // -h, --help
    version: bool,     // -V, --version
    file_names: Vec<String>,
}

enum TrashRootType {
    Home,        // trash directory is in user's home directory
    TopDirAdmin, // trash directory is the .Trash/{euid} directory in the top directory for the mount the file exists in
    TopDirUser, // trash directory is the .Trash-{euid} directory in the top directory for the mount the file exists in
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
    fn resolve_for_file(
        abs_file_path: &PathBuf,
        verbose: bool,
    ) -> Result<TrashDirectory, Box<dyn Error>> {
        if verbose {
            msg("deriving trash root");
        }

        // check if the file is in a home mount
        // "To be more precise, from a partition/device different from the one on which $XDG_DATA_HOME resides"
        let xdg_data_home = get_xdg_data_home()?;
        let mut file_dev = Device::for_path(abs_file_path)?;
        let xdg_data_home_dev = Device::for_path(&xdg_data_home)?;
        let trash_root_type: TrashRootType;

        let trash_home = if file_dev.dev_num.dev_id == xdg_data_home_dev.dev_num.dev_id {
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
            file_dev.resolve_mount()?;
            let top_dir = file_dev.mount_point.clone().unwrap();

            // user specific directory name
            // using effective uid to use root trash if invoked with sudo
            // if real uid is used, restoring will have trouble with permissions
            // todo: int test with a another user
            let euid: u32;
            unsafe {
                euid = libc::geteuid();
            }

            let trash_home = match Self::try_topdir_admin_trash(top_dir.clone(), euid) {
                Ok(v) => {
                    trash_root_type = TrashRootType::TopDirAdmin;
                    v
                }
                Err(e) => {
                    // if the method (1) fails at any point — that is, the $topdir/.
                    // Trash directory does not exist, or it fails the checks, or the
                    // system refuses to create an $uid directory in it — the
                    // implementation MUST, by default, fall back to method (2)
                    //
                    // Besides, the implementation SHOULD report the failed
                    // check to the administrator, and MAY also report it to
                    // the user.
                    msg_err(format!("top directory trash for file is unusable: {e}"));

                    let top_dir_user_trash = Self::try_topdir_user_trash(top_dir, euid)?;
                    trash_root_type = TrashRootType::TopDirUser;
                    top_dir_user_trash
                }
            };

            trash_home
        };

        let files_dir = trash_home.join("files");
        must_have_dir(&files_dir)?;

        let info_dir = trash_home.join("info");
        must_have_dir(&info_dir)?;

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
            "reached maximum trash file name iteration",
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
        // check if $topdir/.Trash exist and is usable
        let admin_trash = top_dir.join(".Trash");
        let admin_trash_location = admin_trash.to_str().unwrap();
        match admin_trash.try_exists() {
            Ok(true) => {
                // If this directory is present, the implementation MUST,
                // by default, check for the “sticky bit”.
                // todo: provide superusers to disable this check to
                // support filesystems that don't support sticky bit.
                //
                // The implementation also MUST check that this directory
                // is not a symbolic link.

                // test if user can write to this dir
                if !is_writable_dir(&admin_trash)? {
                    return Err(Box::<dyn Error>::from(format!(
                        "top directory trash '{admin_trash_location}' isn't writable"
                    )));
                }

                // check if sticky bit is set and is not a symlink
                let mode = admin_trash.metadata()?.st_mode();
                let sticky_bit_set = mode & libc::S_ISVTX == libc::S_ISVTX;
                if sticky_bit_set && !admin_trash.is_symlink() {
                    // topdir approach 1
                    //
                    // if this directory does not exist for the current user, the
                    // implementation MUST immediately create it, without any
                    // warnings or delays for the user.
                    //
                    // $topdir/.Trash/$uid
                    let user_trash_home = admin_trash.join(euid.to_string());
                    must_have_dir(&user_trash_home)?;
                    if !is_writable_dir(&user_trash_home)? {
                        let user_trash_location = user_trash_home.to_str().unwrap();
                        return Err(Box::<dyn Error>::from(format!(
                            "user directory in top directory trash '{user_trash_location}' isn't writable"
                        )));
                    }

                    Ok(user_trash_home)
                } else {
                    Err(Box::<dyn Error>::from(format!(
                        "top directory trash '{admin_trash_location}' is a symlink or sticky bit not set",
                    )))
                }
            }
            _ => Err(Box::<dyn Error>::from(format!(
                "top directory trash '{admin_trash_location}' does not exist"
            ))),
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
        if !is_writable_dir(&user_trash_home)? {
            let user_trash_location = user_trash_home.to_str().unwrap();
            return Err(Box::<dyn Error>::from(format!(
                "user directory in top directory trash '{user_trash_location}' isn't writable"
            )));
        }

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

        let mut f = match std::fs::File::create(info_entry) {
            Ok(v) => v,
            Err(e) => {
                return Err(Box::<dyn Error>::from(format!(
                    "error while creating trashinfo entry: {}",
                    e
                )));
            }
        };

        match f.write_all(trashinfo.as_bytes()) {
            Ok(_) => (),
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
        fs::rename(&self.original_file, files_entry)?;
        Ok(files_entry)
    }
}

// retrieve os defined home directory. $HOME MUST be defined as of now.
// todo: lookup passwd for home dir entry if $HOME isn't defined
fn get_home_dir() -> Result<PathBuf, Box<dyn Error>> {
    let home_dir = env::var("HOME")?;
    let home_path = PathBuf::from(&home_dir);

    Ok(home_path)
}

// retrieve XDG_DATA_HOME value, from env var or falling back to spec default
fn get_xdg_data_home() -> Result<PathBuf, Box<dyn Error>> {
    // if XDG_DATA_HOME is not defined, fallback to $HOME/.local/share
    let xdg_data_home = match env::var("XDG_DATA_HOME") {
        Ok(v) => PathBuf::from(&v),
        Err(_) => {
            let home_dir = get_home_dir()
                .map_err(|_| Box::<dyn Error>::from("couldn't retrieve home directory location"));

            home_dir?.join(".local").join("share")
        }
    };

    Ok(xdg_data_home)
}

fn is_writable_dir(path: &PathBuf) -> Result<bool, Box<dyn Error>> {
    let writable: libc::c_int;
    let dir_location = path.to_str().unwrap();
    let path_cstr = CString::new(dir_location)?;
    unsafe {
        writable = libc::access(path_cstr.as_ptr(), libc::W_OK);
    }

    // access manpage for ubuntu: On success (all requested
    // permissions granted, or mode is F_OK and the file exists),
    // zero is returned.
    if writable != 0 {
        return Ok(false);
    }

    Ok(true)
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
                    "cannot create directory: {}, {}",
                    path.to_str().unwrap(),
                    e,
                ))
            });
        }
        Err(_) => {
            return Err(Box::<dyn Error>::from(format!(
                "cannot verify directory exists: {}",
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

fn msg_err<T>(msg: T) -> ()
where
    T: std::fmt::Display,
{
    let binary_name = &*BINARY_NAME;
    eprintln!("{binary_name}: {msg}")
}

fn msg<T>(msg: T) -> ()
where
    T: std::fmt::Display,
{
    let binary_name = &*BINARY_NAME;
    println!("{binary_name}: {msg}")
}
