use chrono::{DateTime, Local};
use rand::Rng;
use std::env;
use std::error::Error;
use std::ffi::CString;
use std::fs::{
    create_dir_all, read_dir, read_to_string, remove_dir_all, remove_file, rename, File,
    OpenOptions,
};
use std::io::Write;
use std::os::linux::fs::MetadataExt;
use std::path::MAIN_SEPARATOR_STR;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use urlencoding::{decode, encode};

// Does NOT support trashing files from external mounts to user's trash dir
// Does NOT trash a file from external mounts to home if topdirs cannot be used

#[derive(Clone, Eq, PartialEq, Debug)]
pub enum TrashRootType {
    Home,        // trash directory is in user's home directory
    TopDirAdmin, // trash directory is the .Trash/{euid} directory in the top directory for the mount the file exists in
    TopDirUser, // trash directory is the .Trash-{euid} directory in the top directory for the mount the file exists in
}

#[derive(Clone)]
pub struct TrashDirectory {
    pub device: Device,
    pub home: PathBuf,
    pub files: PathBuf,
    pub info: PathBuf,
    pub root_type: TrashRootType,
}

impl TrashDirectory {
    pub fn from(
        root: PathBuf,
        device: Device,
        root_type: TrashRootType,
    ) -> Result<Self, Box<dyn Error>> {
        let files_dir = root.join("files");
        must_have_dir(&files_dir)?;

        let info_dir = root.join("info");
        must_have_dir(&info_dir)?;

        Ok(TrashDirectory {
            device,
            root_type,
            home: root,
            files: files_dir,
            info: info_dir,
        })
    }

    // derive trash directory according to trash spec
    // does not traverse symlinks
    // todo: support expunge dir (not sure how to schedule job for permanent deletion)
    pub fn resolve_for_file(
        abs_file_path: &Path,
        verbose: bool,
    ) -> Result<TrashDirectory, Box<dyn Error>> {
        if verbose {
            msg("deriving trash root");
        }

        // check if the file is in a home mount
        // "To be more precise, from a partition/device different from the one on which $XDG_DATA_HOME resides"
        let xdg_data_home = get_xdg_data_home()?;
        must_have_dir(&xdg_data_home)?;
        if verbose {
            msg("deriving file device");
        }
        let mut file_dev = Device::for_path(abs_file_path)?;
        let xdg_data_home_dev = Device::for_path(&xdg_data_home)?;
        let trash_root_type: TrashRootType;

        if verbose {
            msg(format!(
                "devices: {} == {}",
                file_dev.dev_num.dev_id, xdg_data_home_dev.dev_num.dev_id
            ));
        }
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

            match Self::try_topdir_admin_trash_for(&top_dir.clone(), euid, true) {
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

                    let top_dir_user_trash = Self::try_topdir_user_trash_for(&top_dir, euid, true)?;
                    trash_root_type = TrashRootType::TopDirUser;
                    top_dir_user_trash
                }
            }
        };

        if verbose {
            msg(format!("root type: {:#?}", trash_root_type));
        }

        let files_dir = trash_home.join("files");
        must_have_dir(&files_dir)?;

        let info_dir = trash_home.join("info");
        must_have_dir(&info_dir)?;

        Ok(TrashDirectory {
            device: file_dev,
            root_type: trash_root_type,
            home: trash_home,
            files: files_dir,
            info: info_dir,
        })
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

            // we've found a fresh number!!
            if !file.exists() && !trashinfo.exists() {
                trash_file.files_entry = Some(file);

                // derive trashinfo entries
                let relative_path: PathBuf;
                // The system SHOULD support absolute pathnames only in the
                // “home trash” directory, not in the directories under $topdir
                let file_path_key = match self.root_type {
                    TrashRootType::Home => trash_file.original_file.to_str().unwrap(),
                    _ => {
                        let trash_home_mt_point = self.device.mount_point.as_ref().unwrap();
                        relative_path =
                            get_path_relative_to(&trash_file.original_file, trash_home_mt_point)?;
                        relative_path.to_str().unwrap()
                    }
                };

                let now = Local::now();
                let trashinfo_entry = TrashInfo::new(trashinfo, file_path_key, now);
                trash_file.trashinfo = Some(trashinfo_entry);

                return Ok(());
            }
        }

        Err(Box::<dyn Error>::from(
            "reached maximum trash file name iteration",
        ))
    }

    // get this trash directory's directorysizes file as a PathBuf
    // if it doesn't exist as a file or a working symlink, an Error is returned
    // if there is no existing file or working symlink, this will create an empty file
    pub fn get_dirsizes_path(&self) -> Result<PathBuf, Box<dyn Error>> {
        let dir_sizes_file = self.home.join("directorysizes");
        match dir_sizes_file.try_exists() {
            Ok(true) => {
                if !dir_sizes_file.is_file() {
                    return Err(Box::<dyn Error>::from(format!(
                        "{} is not a file, not updating directorysizes",
                        dir_sizes_file.display(),
                    )));
                }

                // this will skip updating dirsizes in topdir trash created by admin
                // (scenario 1). It's easier to keep this behavior consistent than being
                // dependent on dir permissions that the user will or will not know about
                if !can_delete_file(&dir_sizes_file) {
                    return Err(Box::<dyn Error>::from(
                        "not enough permissions to edit directorysizes",
                    ));
                }
                Ok(dir_sizes_file)
            }
            Ok(false) => {
                if dir_sizes_file.is_symlink() {
                    return Err(Box::<dyn Error>::from(format!(
                        "{} is a broken symlink",
                        dir_sizes_file.display()
                    )));
                }

                let mut f = File::create(dir_sizes_file.clone())?;
                f.write_all(b"")?;

                Ok(dir_sizes_file)
            }
            Err(e) => Err(Box::new(e)),
        }
    }

    pub fn add_dirsizes_entry(&self, trash_file: &TrashFile) -> Result<(), Box<dyn Error>> {
        if trash_file.files_entry.is_none() {
            return Err(Box::<dyn Error>::from(
                "attempt to update directorysizes for incomplete trash operation",
            ));
        }

        let trashed_file = trash_file.files_entry.clone().unwrap();
        if !trashed_file.is_dir() {
            return Ok(());
        }

        let current_dir_sizes = self.get_dirsizes_path()?;

        let size = get_dir_size(&trashed_file)?;
        let mtime = match trash_file
            .trashinfo
            .clone()
            .unwrap()
            .path
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
        let mut existing_content = if current_dir_sizes.metadata()?.st_size() != 0 {
            let mut existing_content: String = String::new();
            let existing_dir_sizes = read_to_string(current_dir_sizes.to_str().unwrap())?;
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

    // todo: duplicate logic from the above, maybe an optional trashfile arg
    pub fn cleanup_dirsizes(&self) -> Result<(), Box<dyn Error>> {
        let current_dir_sizes = self.get_dirsizes_path()?;
        if current_dir_sizes.metadata()?.st_size() == 0 {
            return Ok(());
        }

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
        let mut existing_content: String = String::new();
        let existing_dir_sizes = read_to_string(current_dir_sizes.to_str().unwrap())?;
        let entries: Vec<&str> = existing_dir_sizes.lines().collect();
        for entry in entries {
            let fields: Vec<&str> = entry.split_whitespace().collect();
            if fields.len() == 3 {
                let f = match decode(fields[2]) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let f_path = self.files.join(f.into_owned());
                if f_path.exists() {
                    existing_content += &format!("{entry}\n").to_string();
                }
            }
        }

        // update with the latest entry
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
    pub fn get_trashed_files(&self) -> Result<Vec<TrashFile>, Box<dyn Error>> {
        let files_dir = self.files.clone();
        let mut files: Vec<TrashFile> = vec![];
        for child in read_dir(files_dir)? {
            let child = child?;
            let child_path = child.path();
            let trash_entry = TrashFile::from(child_path, self)?;
            files.push(trash_entry);
        }

        Ok(files)
    }

    pub fn get_all_trash_roots() -> Result<Vec<TrashDirectory>, Box<dyn Error>> {
        // filter /proc/mounts
        let mounts_content = read_to_string("/proc/mounts")?;
        let mounts: Vec<&str> = mounts_content.lines().collect();

        let mut trash_roots: Vec<TrashDirectory> = vec![];
        for mount in mounts {
            let fields: Vec<&str> = mount.split_whitespace().collect();
            // drop if device not in /dev
            // drop if device is /dev/loop* (snap if present)
            let device = fields[0];
            if !device.starts_with("/dev") || device.starts_with("/dev/loop") {
                continue;
            }

            // drop if mounted to /boot, typically not used for external trashing
            let mount_root = fields[1];
            if mount_root.starts_with("/boot") {
                continue;
            }

            // drop if trashroot not present
            let mount_path = PathBuf::from(mount_root);
            let euid: u32;
            unsafe {
                euid = libc::geteuid();
            }

            match TrashDirectory::topdir_admin_trash_exists_for(&mount_path, euid) {
                Ok(v) => {
                    let abs_path = to_abs_path(&mount_path)?;
                    let dev = Device::for_path(&abs_path)?;
                    let trash_dir = TrashDirectory::from(v, dev, TrashRootType::TopDirAdmin)?;
                    trash_roots.push(trash_dir);
                }
                Err(_) => match TrashDirectory::topdir_user_trash_exists_for(&mount_path, euid) {
                    Ok(v) => {
                        let abs_path = to_abs_path(&mount_path)?;
                        let dev = Device::for_path(&abs_path)?;
                        let trash_dir = TrashDirectory::from(v, dev, TrashRootType::TopDirUser)?;
                        trash_roots.push(trash_dir);
                    }
                    Err(_) => continue,
                },
            };
        }

        Ok(trash_roots)
    }

    // get a unique file name suffix to file the potential trash file under
    //
    // files/directories with the same name can be trashed from difference
    // sources (or even from the same source).This should be handled without
    // exposing the details to the user
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

    pub fn topdir_admin_trash_exists_for(
        top_dir: &Path,
        euid: libc::uid_t,
    ) -> Result<PathBuf, Box<dyn Error>> {
        TrashDirectory::try_topdir_admin_trash_for(top_dir, euid, false)
    }

    pub fn try_topdir_admin_trash_for(
        top_dir: &Path,
        euid: libc::uid_t,
        create_if_not_exist: bool,
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
                    if create_if_not_exist {
                        must_have_dir(&user_trash_home)?;
                    } else if !user_trash_home.try_exists().unwrap_or(false) {
                        return Err(Box::<dyn Error>::from(format!(
                            "user directory in top directory trash '{}' isn't writable",
                            user_trash_home.display(),
                        )));
                    }

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

    pub fn topdir_user_trash_exists_for(
        top_dir: &Path,
        euid: libc::uid_t,
    ) -> Result<PathBuf, Box<dyn Error>> {
        TrashDirectory::try_topdir_user_trash_for(top_dir, euid, false)
    }

    pub fn try_topdir_user_trash_for(
        top_dir: &Path,
        euid: libc::uid_t,
        create_if_not_exist: bool,
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
        if create_if_not_exist {
            must_have_dir(&user_trash_home)?;
        } else if !user_trash_home.try_exists().unwrap_or(false) {
            return Err(Box::<dyn Error>::from(format!(
                "user directory in top directory trash '{}' isn't writable",
                user_trash_home.display(),
            )));
        }

        if !is_writable_dir(&user_trash_home) {
            let user_trash_location = user_trash_home.to_str().unwrap();
            return Err(Box::<dyn Error>::from(format!(
                "user directory in top directory trash '{user_trash_location}' isn't writable"
            )));
        }

        Ok(user_trash_home)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrashInfo {
    pub original_path: String, // encoded path entry
    pub deletion_date: String, // formatted date
    pub path: PathBuf,
}

impl TrashInfo {
    pub fn new(trashinfo: PathBuf, original_path: &str, deletion_date: DateTime<Local>) -> Self {
        // SHOULD store the file name as the sequence of bytes
        // produced by the file system, with characters escaped as in
        // URLs (as defined by RFC 2396, section 2)
        let file_path_encoded = &encode(original_path);

        // are to be in the YYYY-MM-DDThh:mm:ss format (see RFC 3339).
        // The time zone should be the user's (or filesystem's) local time
        let mut deletion_date_fmt =
            deletion_date.to_rfc3339_opts(chrono::format::SecondsFormat::Secs, true);
        // drop everything after + or Z
        for offset_char in ["+", "z", "Z"] {
            let tz_offset = deletion_date_fmt
                .find(offset_char)
                .unwrap_or(deletion_date_fmt.len());
            deletion_date_fmt.replace_range(tz_offset.., "");
        }

        TrashInfo {
            original_path: file_path_encoded.to_string(),
            deletion_date: deletion_date_fmt,
            path: trashinfo,
        }
    }

    pub fn from(path: &PathBuf) -> Result<Self, Box<dyn Error>> {
        let trashinfo_content = read_to_string(path).expect("couldn't read trashinfo entry");
        let lines: Vec<&str> = trashinfo_content.split("\n").collect();

        if lines[0].trim() != "[Trash Info]"
            || !lines[1].starts_with("Path=")
            || !lines[2].starts_with("DeletionDate=")
        {
            return Err(Box::<dyn Error>::from("not a valid trashinfo entry"));
        }

        let original_path = &lines[1]["Path=".len()..];
        let deletion_date = &lines[2]["DeletionDate=".len()..];

        Ok(TrashInfo {
            original_path: original_path.to_string(),
            deletion_date: deletion_date.to_string(),
            path: path.to_path_buf(),
        })
    }

    pub fn get_original_path(&self) -> PathBuf {
        PathBuf::from(decode(&self.original_path).expect("utf-8").into_owned())
    }

    pub fn create_file(&self) -> Result<&PathBuf, Box<dyn Error>> {
        if self.path.exists() {
            return Err(Box::<dyn Error>::from("info entry already exists"));
        }

        let trashinfo = format!(
            r#"[Trash Info]
Path={}
DeletionDate={}
"#,
            self.original_path, self.deletion_date
        );

        let mut f = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.path)
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

        Ok(&self.path)
    }

    pub fn get_deletion_date(&self) -> DateTime<Local> {
        // sometimes deletion date has tz info because of a bug from a previous commit
        // drop everything after + or Z
        let mut deletion_date = self.deletion_date.clone();
        for offset_char in ["+", "z", "Z"] {
            let tz_offset = deletion_date
                .find(offset_char)
                .unwrap_or(deletion_date.len());
            deletion_date.replace_range(tz_offset.., "");
        }

        // assume user/machine local tz
        let now = Local::now();
        let diff_mins: i32 = now.offset().local_minus_utc() / 60;
        let diff_hours: i32 = diff_mins / 60;
        let diff_mins_remaining = diff_mins % 60;
        let deletion_datetime = format!("{deletion_date}+{diff_hours:02}:{diff_mins_remaining:02}");
        // println!("diff_mins: {diff_mins}, diff_hours: {diff_hours}, diff_mins_rem: {diff_mins_remaining}, date: {deletion_datetime}");

        deletion_datetime.parse().unwrap()
    }
}

pub struct TrashFile {
    pub original_file: PathBuf,
    pub files_entry: Option<PathBuf>,
    pub trashinfo: Option<TrashInfo>,
    pub trashroot: TrashDirectory,
}

impl TrashFile {
    // file to be trashed
    pub fn new(
        original_file: PathBuf,
        trashroot: &TrashDirectory,
    ) -> Result<TrashFile, Box<dyn Error>> {
        if !original_file.is_absolute() {
            return Err(Box::<dyn Error>::from("file path is not absolute"));
        }

        Ok(TrashFile {
            original_file,
            files_entry: None,
            trashinfo: None,
            trashroot: trashroot.clone(),
        })
    }

    // from existing file
    pub fn from(
        trash_file: PathBuf,
        trash_dir: &TrashDirectory,
    ) -> Result<TrashFile, Box<dyn Error>> {
        let trashinfo_path = trash_dir.info.join(format!(
            "{}.trashinfo",
            trash_file.file_name().unwrap().to_str().unwrap()
        ));
        if !trashinfo_path.is_file() {
            return Err(Box::<dyn Error>::from("trash file has no trashinfo entry"));
        }

        let trashinfo = TrashInfo::from(&trashinfo_path)?;
        let original_file = trashinfo.get_original_path();
        let trash_entry = TrashFile {
            original_file,
            files_entry: Some(trash_file),
            trashinfo: Some(trashinfo),
            trashroot: trash_dir.clone(),
        };

        Ok(trash_entry)
    }

    pub fn create_trashinfo(&self) -> Result<&PathBuf, Box<dyn Error>> {
        if self.files_entry.is_none() || self.trashinfo.is_none() {
            return Err(Box::<dyn Error>::from("trash entries are uninitialised"));
        }

        self.trashinfo.as_ref().unwrap().create_file()
    }

    pub fn trash(&self) -> Result<&PathBuf, Box<dyn Error>> {
        if self.files_entry.is_none() || self.trashinfo.is_none() {
            return Err(Box::<dyn Error>::from("trash entries are uninitialised"));
        }

        rename(&self.original_file, self.files_entry.as_ref().unwrap())?;

        let is_dir = !self.files_entry.as_ref().unwrap().is_symlink()
            && self.files_entry.as_ref().unwrap().is_dir();
        if is_dir {
            // doesn't matter if this fails
            let _ = self.trashroot.add_dirsizes_entry(self);
        }

        Ok(self.files_entry.as_ref().unwrap())
    }

    pub fn restore(&self) -> Result<&PathBuf, Box<dyn Error>> {
        if self.files_entry.is_none() || self.trashinfo.is_none() {
            return Err(Box::<dyn Error>::from("trash entries are uninitialised"));
        }

        let is_dir = !self.files_entry.as_ref().unwrap().is_symlink()
            && self.files_entry.as_ref().unwrap().is_dir();

        rename(self.files_entry.as_ref().unwrap(), &self.original_file)?;
        remove_file(&self.trashinfo.as_ref().unwrap().path)?;

        // if dir, remvoe from dir sizes
        if is_dir {
            // doesn't matter if this fails
            let _ = self.trashroot.cleanup_dirsizes();
        }

        Ok(&self.original_file)
    }

    pub fn delete_forever(&self) -> Result<(), Box<dyn Error>> {
        if self.files_entry.is_none() || self.trashinfo.is_none() {
            return Err(Box::<dyn Error>::from("trash entries are uninitialised"));
        }

        let is_dir = !self.files_entry.as_ref().unwrap().is_symlink()
            && self.files_entry.as_ref().unwrap().is_dir();

        if is_dir {
            remove_dir_all(self.files_entry.as_ref().unwrap())?;
        } else {
            remove_file(self.files_entry.as_ref().unwrap())?;
        }

        remove_file(&self.trashinfo.as_ref().unwrap().path)?;

        // if dir, remvoe from dir sizes
        if is_dir {
            self.trashroot.cleanup_dirsizes()?;
        }

        Ok(())
    }

    // size in bytes (not the size on disk)
    pub fn get_size(&self) -> Result<u64, Box<dyn Error>> {
        if self.files_entry.is_none() || self.trashinfo.is_none() {
            return Err(Box::<dyn Error>::from("trash entries are uninitialised"));
        }

        let size = if self.files_entry.as_ref().unwrap().is_symlink() {
            self.files_entry
                .as_ref()
                .unwrap()
                .symlink_metadata()
                .unwrap()
                .st_size()
        } else if self.files_entry.as_ref().unwrap().is_dir() {
            get_dir_size(self.files_entry.as_ref().unwrap())?
        } else {
            self.files_entry
                .as_ref()
                .unwrap()
                .metadata()
                .unwrap()
                .st_size()
        };

        Ok(size)
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
pub fn is_writable_dir(path: &Path) -> bool {
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
// if the path doesn't exist, the directory is created, including the parent
// paths.
// if parent paths cannot be created, an error is returned
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
            return create_dir_all(path).map_err(|e| {
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
pub fn get_path_relative_to(child: &Path, parent: &PathBuf) -> Result<PathBuf, Box<dyn Error>> {
    if !child.is_absolute() || !parent.is_absolute() {
        return Err(Box::<dyn Error>::from("require absolute paths"));
    }

    let stripped = child.strip_prefix(parent)?;
    Ok(stripped.to_path_buf())
}

// check permissions for a file/directory to be deleted without dereferencing if a symlink
// if absolute file path is not provided, treated as relative to the current working directory
pub fn can_delete_file(abs_file_path: &Path) -> bool {
    // 1. can delete? - user needs to have rwx for the parent dir
    let parent = match abs_file_path.parent() {
        Some(v) => v,
        None => return false,
    };

    if !is_writable_dir(parent) {
        return false;
    }

    // 1. can read and modify?
    let file_writable: libc::c_int;
    let location = abs_file_path.to_str().unwrap();
    let path_cstr = match CString::new(location) {
        Ok(v) => v,
        Err(_) => return false,
    };
    unsafe {
        file_writable = libc::faccessat(
            libc::AT_FDCWD, // relative to cwd
            path_cstr.as_ptr(),
            libc::R_OK | libc::W_OK,
            libc::AT_SYMLINK_NOFOLLOW, // do not dereference symlinks
        );
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
            if !child_path.is_symlink() & child_path.is_dir() {
                total_size += get_dir_size(&child_path)?;
            } else if !child_path.is_symlink() && child_path.is_file() {
                let block_count = child_path.metadata()?.st_blocks();
                total_size += block_count * 512;
            }
        }
    } else {
        return Err(Box::<dyn Error>::from("path is not a directory"));
    }

    Ok(total_size)
}

#[derive(Clone)]
pub struct Device {
    pub dev_num: DeviceNumber,
    dev_name: Option<String>, // only available for external mounts
    mount_root: Option<PathBuf>,
    mount_point: Option<PathBuf>,
}

impl Device {
    // man 5 proc
    const PROCINFO_FIELD_MAJORMINOR: usize = 2;
    const PROCINFO_FIELD_MOUNT_ROOT: usize = 3;
    const PROCINFO_FIELD_MOUNT_POINT: usize = 4;
    const PROCINFO_FIELD_DEV_NAME: usize = 9;

    // does not traverse symlinks
    pub fn for_path(abs_file_path: &Path) -> Result<Device, Box<dyn Error>> {
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

#[derive(Clone)]
pub struct DeviceNumber {
    pub dev_id: u64,
    major: u32,
    minor: u32,
}

impl DeviceNumber {
    // does not traverse symlinks
    // latest device drivers ref - Ch3
    // Within the kernel, the dev_t type (defined in <linux/types.h>) is used to hold device
    // numbers—both the major and minor parts. As of Version 2.6.0 of the kernel, dev_t is
    // a 32-bit quantity with 12 bits set aside for the major number and 20 for the minor
    // number. Your code should, of course, never make any assumptions about the inter-
    // nal organization of device numbers;
    pub fn for_path(abs_file_path: &Path) -> Result<DeviceNumber, Box<dyn Error>> {
        let f_metadata = if abs_file_path.is_symlink() {
            abs_file_path.symlink_metadata()?
        } else {
            abs_file_path.metadata()?
        };

        let file_device_id = f_metadata.st_dev();

        let major: u32;
        let minor: u32;

        unsafe {
            major = libc::major(file_device_id);
            minor = libc::minor(file_device_id);
        }

        let dev_number = DeviceNumber {
            dev_id: file_device_id,
            major,
            minor,
        };

        Ok(dev_number)
    }
}

// convert a file path to absolute path WITHOUT Traversing symlinks
// does not check if path exists
// errors - from current_dir() call
// todo: test, shamelessly stolen from so
pub fn to_abs_path(path: impl AsRef<Path>) -> Result<PathBuf, Box<dyn Error>> {
    let path = path.as_ref();
    let abs_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        // ex: if starts with ./, remove it because that looks ugly
        let rel_indicator = format!(".{MAIN_SEPARATOR_STR}");
        let trimmed_path = if path.starts_with(&rel_indicator) {
            let t = path.display().to_string();
            &PathBuf::from(t.strip_prefix(&rel_indicator).unwrap())
        } else {
            path
        };

        env::current_dir()?.join(trimmed_path)
    };

    Ok(abs_path)
}

pub fn msg_err<T>(msg: T)
where
    T: std::fmt::Display,
{
    let binary_name = env!("CARGO_PKG_NAME");
    eprintln!("{binary_name}: {msg}")
}

pub fn msg<T>(msg: T)
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
