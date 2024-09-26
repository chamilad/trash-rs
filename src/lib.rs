use chrono::Local;
use rand::Rng;
use std::env;
use std::error::Error;
use std::ffi::CString;
use std::fs::{create_dir, read_dir, read_to_string, rename, File, OpenOptions};
use std::io::Write;
use std::os::linux::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use urlencoding::{decode, encode};

use libc;

// Does NOT support trashing files from external mounts to user's trash dir
// Does NOT trash a file from external mounts to home if topdirs cannot be used

#[derive(Eq, PartialEq)]
pub enum TrashRootType {
    Home,        // trash directory is in user's home directory
    TopDirAdmin, // trash directory is the .Trash/{euid} directory in the top directory for the mount the file exists in
    TopDirUser, // trash directory is the .Trash-{euid} directory in the top directory for the mount the file exists in
}

pub struct TrashDirectory {
    pub device: Device,
    pub home: PathBuf,
    pub files: PathBuf,
    pub info: PathBuf,
    pub root_type: TrashRootType,
}

impl TrashDirectory {
    // derive trash directory according to trash spec
    // todo: support expunge dir (not sure how to schedule job for permanent deletion)
    pub fn resolve_for_file(
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
        };

        Ok(trash_dir)
    }

    pub fn generate_trash_entry_names(
        &self,
        trash_file: &mut TrashFile,
    ) -> Result<(), Box<dyn Error>> {
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

    pub fn update_dir_sizes_entry(&self, trash_file: &TrashFile) -> Result<(), Box<dyn Error>> {
        if trash_file.files_entry.is_none() {
            return Err(Box::<dyn Error>::from(
                "attempt to update directorysizes for incomplete trash operation",
            ));
        }

        let trashed_file = trash_file.files_entry.clone().unwrap();
        if !trashed_file.is_dir() {
            return Ok(());
        }

        let current_dir_sizes = self.home.join("directorysizes");
        // this will skip updating dirsizes in topdir trash created by admin
        // (scenario 1). It's easier to keep this behavior consistent than being
        // dependent on dir permissions that the user will or will not know about
        if !can_delete_file(&current_dir_sizes) {
            return Err(Box::<dyn Error>::from(
                "not enough permissions to edit directorysizes",
            ));
        }

        if current_dir_sizes.exists() && !current_dir_sizes.is_file() {
            let p = current_dir_sizes.to_str().unwrap();
            return Err(Box::<dyn Error>::from(format!(
                "{p} is not a file, not updating directorysizes"
            )));
        }

        let size = get_dir_size(&trashed_file)?;
        let mtime = match trash_file
            .trashinfo_entry
            .clone()
            .unwrap()
            .metadata()
            .unwrap()
            .modified()
        {
            Ok(v) => v,
            Err(e) => {
                msg_err(format!(
                    "cannot update directorysizes: cannot get mtime: {e}"
                ));
                return Ok(());
            }
        };

        let mtime_epoch = mtime.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();

        // encode the dir name
        let dir_name = trashed_file.file_name().unwrap().to_str().unwrap();
        let encoded_dir_name = encode(dir_name);

        let mut rng = rand::thread_rng();
        let random_nu = rng.gen_range(100000000..999999999);

        // below rename will not work across different mount points
        // so the temp file has to be made on the same parition
        let temp_dir = if self.root_type == TrashRootType::Home {
            env::temp_dir()
        } else {
            self.home.clone()
        };

        let binary_name = env!("CARGO_PKG_NAME");
        let tool_temp_dir = temp_dir.join(binary_name);
        must_have_dir(&tool_temp_dir)?;

        let target_file_path = tool_temp_dir.join(format!("directorysizes-{random_nu}"));

        // cleanup existing entries if other implementations do not support this
        // part of the spec. If this isn't done, directorysizes keeps on growing
        let mut existing_content = if current_dir_sizes.exists() {
            let mut existing_content: String = String::new();
            let existing_dir_sizes = read_to_string(&current_dir_sizes.to_str().unwrap())?;
            let trash_file_path = trash_file.files_entry.clone().unwrap();
            let trash_file_name = trash_file_path.file_name().unwrap().to_str().unwrap();
            let entries: Vec<&str> = existing_dir_sizes.lines().collect();
            for entry in entries {
                let fields: Vec<&str> = entry.split_whitespace().collect();
                if fields.len() == 3 {
                    let f = match decode(fields[2]) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    // the directory being trashed could be one that was
                    // trashed before and then restored by an implementation
                    // that does not use directorysizes. In that case, the dir
                    // name for the files directory could be the same as the
                    // previous one. For this case, the dirsizes entry should
                    // not be preserved. The next condition which checks for
                    // existence isn't going to be useful, because at this
                    // point, the directory has already been moved
                    // to the trash bin.
                    if trash_file_name == f {
                        continue;
                    }

                    let f_path = self.files.join(f.into_owned());
                    if f_path.exists() {
                        existing_content += &format!("{entry}\n").to_string();
                    }
                }
            }

            existing_content
        } else {
            String::new()
        };

        // update with the latest entry
        existing_content += &format!("{size} {mtime_epoch} {encoded_dir_name}\n").to_string();
        let mut f = File::create(&target_file_path)?;
        if let Err(e) = f.write_all(existing_content.as_bytes()) {
            return Err(Box::<dyn Error>::from(format!(
                "couldn't update directorysizes: {e}"
            )));
        }

        // atomically move the file back
        rename(&target_file_path, &current_dir_sizes)?;
        Ok(())
    }

    pub fn get_trashable_file_name(stripped_file_name: String, idx: u32) -> String {
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

    pub fn try_topdir_admin_trash(
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
                if !is_writable_dir(&admin_trash) {
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
                    if !is_writable_dir(&user_trash_home) {
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

    pub fn try_topdir_user_trash(
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
        if !is_writable_dir(&user_trash_home) {
            let user_trash_location = user_trash_home.to_str().unwrap();
            return Err(Box::<dyn Error>::from(format!(
                "user directory in top directory trash '{user_trash_location}' isn't writable"
            )));
        }

        Ok(user_trash_home)
    }
}

pub struct TrashFile {
    original_file: PathBuf,
    // file_type: FileType,
    files_entry: Option<PathBuf>,
    trashinfo_entry: Option<PathBuf>,
}

impl TrashFile {
    pub fn new(original_file: PathBuf) -> Result<TrashFile, Box<dyn Error>> {
        if !original_file.is_absolute() {
            return Err(Box::<dyn Error>::from("file path is not absolute"));
        }

        Ok(TrashFile {
            original_file,
            files_entry: None,
            trashinfo_entry: None,
        })
    }

    pub fn create_trashinfo(&self, trash_dir: &TrashDirectory) -> Result<&PathBuf, Box<dyn Error>> {
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

        let file_path_encoded = &encode(file_path_key);

        let info_entry = self.trashinfo_entry.as_ref().unwrap();
        if info_entry.exists() {
            return Err(Box::<dyn Error>::from("info entry already exists"));
        }

        let now = Local::now();
        let deletion_date = now.to_rfc3339_opts(chrono::format::SecondsFormat::Secs, true);
        let trashinfo = format!(
            r#"[Trash Info]
Path={}
DeletionDate={}
"#,
            file_path_encoded, deletion_date
        );

        let mut f = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(info_entry)
        {
            Ok(v) => v,
            Err(e) => {
                return Err(Box::<dyn Error>::from(format!(
                    "error while creating trashinfo entry: {e}"
                )));
            }
        };

        match f.write_all(trashinfo.as_bytes()) {
            Ok(_) => (),
            Err(e) => {
                return Err(Box::<dyn Error>::from(format!(
                    "error while writing to trashinfo file: {e}"
                )));
            }
        };

        Ok(info_entry)
    }

    pub fn trash(&self) -> Result<&PathBuf, Box<dyn Error>> {
        if self.files_entry == None || self.trashinfo_entry == None {
            return Err(Box::<dyn Error>::from("trash entries are uninitialised"));
        }

        let files_entry = self.files_entry.as_ref().unwrap();
        rename(&self.original_file, files_entry)?;
        Ok(files_entry)
    }
}

// retrieve os defined home directory. $HOME MUST be defined as of now.
// todo: lookup passwd for home dir entry if $HOME isn't defined
pub fn get_home_dir() -> Result<PathBuf, Box<dyn Error>> {
    let home_dir = env::var("HOME")?;
    let home_path = PathBuf::from(&home_dir);

    Ok(home_path)
}

// retrieve XDG_DATA_HOME value, from env var or falling back to spec default
pub fn get_xdg_data_home() -> Result<PathBuf, Box<dyn Error>> {
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

// todo: this check is done with process real uid, so sudo invocation will still fail
// alternative is to use faccessat() with AT_EACCESS.
// the decision here is to whether allow sudo invocation to trash a file that
// a user doesn't have access to
pub fn is_writable_dir(path: &PathBuf) -> bool {
    let writable: libc::c_int;
    let dir_location = path.to_str().unwrap();
    let path_cstr = match CString::new(dir_location) {
        Ok(v) => v,
        Err(_) => return false,
    };
    unsafe {
        writable = libc::access(path_cstr.as_ptr(), libc::R_OK | libc::W_OK | libc::X_OK);
    }

    // access manpage for ubuntu: On success (all requested
    // permissions granted, or mode is F_OK and the file exists),
    // zero is returned.
    if writable != 0 {
        return false;
    }

    true
}

// make sure the specified path exists as a directory.
// if the path doesn't exist, the directory is created.
// if it exists and is not a directory, an Error is returned
pub fn must_have_dir(path: &PathBuf) -> Result<(), Box<dyn Error>> {
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
            return create_dir(path).map_err(|e| {
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
pub fn get_path_relative_to(child: &PathBuf, parent: &PathBuf) -> Result<PathBuf, Box<dyn Error>> {
    if !child.is_absolute() || !parent.is_absolute() {
        return Err(Box::<dyn Error>::from("require absolute paths"));
    }

    let stripped = child.strip_prefix(parent)?;
    Ok(stripped.to_path_buf())
}

pub fn can_delete_file(file_path: &PathBuf) -> bool {
    // 1. can delete? - user needs to have rwx for the parent dir
    let parent = match file_path.parent() {
        Some(v) => v,
        None => return false,
    };

    if !is_writable_dir(&parent.to_path_buf()) {
        return false;
    }

    // 1. can read and modify?
    let file_writable: libc::c_int;
    let location = file_path.to_str().unwrap();
    let path_cstr = match CString::new(location) {
        Ok(v) => v,
        Err(_) => return false,
    };
    unsafe {
        file_writable = libc::access(path_cstr.as_ptr(), libc::R_OK | libc::W_OK);
    }

    if file_writable != 0 {
        return false;
    }

    true
}

// symlinks excluded
// same as du -B1 command
// spec: The size is calculated as the disk space used by the directory and
// its contents, that is, the size of the blocks, in bytes (in the same way
// as the `du -B1` command calculates).
pub fn get_dir_size(path: &PathBuf) -> Result<u64, Box<dyn Error>> {
    let mut total_size: u64 = 0;
    if path.is_dir() {
        // calculate dir metadata size
        let block_count = path.metadata()?.st_blocks();
        total_size += block_count * 512;

        for child in read_dir(path)? {
            let child = child?;
            let child_path = child.path();
            if child_path.is_dir() {
                total_size += get_dir_size(&child_path)?;
            } else if child_path.is_file() && !child_path.is_symlink() {
                let block_count = child_path.metadata()?.st_blocks();
                total_size += block_count * 512;
            }
        }
    } else {
        return Err(Box::<dyn Error>::from("path is not a directory"));
    }

    Ok(total_size)
}

pub struct Device {
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

    pub fn for_path(abs_file_path: &PathBuf) -> Result<Device, Box<dyn Error>> {
        let dev_id = DeviceNumber::for_path(abs_file_path)?;
        Ok(Device {
            dev_num: dev_id,
            dev_name: None,
            mount_root: None,
            mount_point: None,
        })
    }

    pub fn resolve_mount(&mut self) -> Result<(), Box<dyn Error>> {
        let mountinfo = read_to_string("/proc/self/mountinfo").unwrap();
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

pub struct DeviceNumber {
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
    pub fn for_path(abs_file_path: &PathBuf) -> Result<DeviceNumber, Box<dyn Error>> {
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

pub fn to_abs_path(path: impl AsRef<Path>) -> Result<PathBuf, Box<dyn Error>> {
    let path = path.as_ref();
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };

    Ok(abs_path)
}

pub fn msg_err<T>(msg: T) -> ()
where
    T: std::fmt::Display,
{
    let binary_name = env!("CARGO_PKG_NAME");
    eprintln!("{binary_name}: {msg}")
}

pub fn msg<T>(msg: T) -> ()
where
    T: std::fmt::Display,
{
    let binary_name = env!("CARGO_PKG_NAME");
    println!("{binary_name}: {msg}")
}

#[cfg(test)]
mod tests {
    use std::fs::create_dir_all;
    use std::fs::remove_dir_all;
    use std::process::Command;

    use super::*;

    #[test]
    fn test_get_dir_size() {
        let temp_dir = env::temp_dir();
        let time_now = match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
            Ok(v) => v.as_secs(),
            Err(_) => panic!("prepare for war"),
        };
        let temp_test_dir = temp_dir.join("trash-rs").join(format!("{}", time_now));
        let test_dir_1 = temp_test_dir.join("test-1");
        let test_dir_2 = temp_test_dir.join("test-2");
        let _ = create_dir_all(test_dir_1.clone());
        let _ = create_dir_all(test_dir_2);

        let test_file = test_dir_1.join("test_file");
        let test_file_size = 10 * 1024 * 1024; // 10MB
        let mut f = File::create(test_file).expect("couldn't create test file");
        let dummy_buffer = vec![0u8; test_file_size];
        let _ = f.write_all(&dummy_buffer);

        let op = Command::new("sh")
            .arg("-c")
            .arg(format!("du -B1 -d 0 {} | cut -f1", temp_test_dir.display()))
            .output()
            .expect("du failed");
        let du_size = String::from_utf8(op.stdout)
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap();
        let dir_size = get_dir_size(&temp_test_dir).unwrap();
        assert!(du_size == dir_size);

        let _ = remove_dir_all(temp_test_dir);
    }
}