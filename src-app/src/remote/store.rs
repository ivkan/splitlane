//! The machine's key and the phones paired with it, on disk.
//!
//! Both live in `remote/` beside the configuration file, readable by the
//! user alone. The key is what a phone's channel is with; whoever reads it
//! can pose as this machine to every paired phone, so it never leaves this
//! directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use splitlane_remote::{DeviceKey, PublicKey, ROOM_LEN, RoomId};

const KEY_FILE: &str = "machine.key";
const DEVICES_FILE: &str = "devices.json";

/// One paired phone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Device {
    /// The relay room the phone and this machine meet in, base64url.
    pub(crate) room: String,
    pub(crate) name: String,
    /// The phone's public key, base64url of the uncompressed point.
    pub(crate) public: String,
    /// Whether the phone may write into agent sessions as well as read.
    pub(crate) reply: bool,
    pub(crate) paired_at_unix: u64,
}

impl Device {
    pub(crate) fn room_id(&self) -> Option<RoomId> {
        let bytes: [u8; ROOM_LEN] = URL_SAFE_NO_PAD.decode(&self.room).ok()?.try_into().ok()?;
        Some(RoomId(bytes))
    }

    pub(crate) fn public_key(&self) -> Option<PublicKey> {
        PublicKey::from_bytes(&URL_SAFE_NO_PAD.decode(&self.public).ok()?).ok()
    }

    pub(crate) fn new(room: RoomId, name: &str, public: &PublicKey, now_unix: u64) -> Self {
        Self {
            room: room.to_text(),
            name: name.to_string(),
            public: URL_SAFE_NO_PAD.encode(public.as_bytes()),
            reply: true,
            paired_at_unix: now_unix,
        }
    }
}

/// Where the files are: `remote/` beside `splitlane.json`.
pub(crate) fn dir() -> Option<PathBuf> {
    let config = splitlane_config::loader::config_path()?;
    Some(config.parent()?.join("remote"))
}

/// The machine's key, made the first time it is asked for.
///
/// Two commands that start at once must not each make one: the phone paired
/// by the one whose key is replaced could never connect again. So a new key
/// is put in place with a link that fails when the file exists, and the
/// loser reads the winner's. And a missing key with phones paired is an
/// error, not a reason to make one: a new key strands every phone.
pub(crate) fn machine_key(dir: &Path) -> io::Result<DeviceKey> {
    let path = dir.join(KEY_FILE);
    let read = |path: &Path| -> io::Result<DeviceKey> {
        DeviceKey::from_bytes(&fs::read(path)?).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })
    };
    match read(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        other => return other,
    }
    if !devices(dir)?.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} is missing while phones are paired with it; remove {} and pair them again",
                path.display(),
                dir.join(DEVICES_FILE).display()
            ),
        ));
    }
    let key = DeviceKey::generate();
    let partial = write_partial(dir, &path, &*key.to_bytes())?;
    let placed = fs::hard_link(&partial, &path);
    let _ = fs::remove_file(&partial);
    match placed {
        Ok(()) => Ok(key),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => read(&path),
        Err(e) => Err(e),
    }
}

/// Keep `device`, replacing an earlier pairing of the same phone: two
/// records of one phone would mean two connections to its room that keep
/// starting each other's channel over.
pub(crate) fn add_device(dir: &Path, device: Device) -> io::Result<()> {
    edit_devices(dir, |all| {
        all.retain(|kept| kept.public != device.public);
        all.push(device);
    })
}

/// Read the paired phones, change them and write them back, one change at a
/// time within this process. Two changes at once would each write what they
/// read, and the later would undo the earlier: a phone paired while another
/// lost its right to reply could hand that right back.
///
/// A `splitlane remote pair` in another process is not covered; the window
/// and the terminal pairing at the same moment is not a case worth a file
/// lock.
fn edit_devices(dir: &Path, change: impl FnOnce(&mut Vec<Device>)) -> io::Result<()> {
    static EDITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one_at_a_time = EDITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut all = devices(dir)?;
    change(&mut all);
    save_devices(dir, &all)
}

/// The record of the phone `paired_as` names, as it stands now: `Ok(None)`
/// once it was removed. Matched on room and key both, so a phone paired
/// again (a new room) is a different phone to a bridge that served the old
/// one. A file that cannot be read is an error, not a removal.
pub(crate) fn device_paired_as(dir: &Path, paired_as: &Device) -> io::Result<Option<Device>> {
    Ok(devices(dir)?
        .into_iter()
        .find(|device| device.room == paired_as.room && device.public == paired_as.public))
}

/// Remove the phone paired in `room`. Its bridge connection leaves the room
/// at its next frame; see `bridge::serve_forever`.
pub(crate) fn remove_device(dir: &Path, room: &str) -> io::Result<()> {
    edit_devices(dir, |all| all.retain(|device| device.room != room))
}

/// Let the phone paired in `room` reply, or only read.
pub(crate) fn set_reply(dir: &Path, room: &str, reply: bool) -> io::Result<()> {
    edit_devices(dir, |all| {
        for device in all.iter_mut().filter(|device| device.room == room) {
            device.reply = reply;
        }
    })
}

/// The paired phones; none when the file is missing.
pub(crate) fn devices(dir: &Path) -> io::Result<Vec<Device>> {
    match fs::read(dir.join(DEVICES_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

pub(crate) fn save_devices(dir: &Path, devices: &[Device]) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(devices).map_err(io::Error::other)?;
    write_private(dir, &dir.join(DEVICES_FILE), &bytes)
}

/// Append `bytes` to `name` in `dir`, creating it readable by the user
/// alone.
pub(crate) fn append_private(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    create_private_dir(dir)?;
    let mut options = fs::OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    io::Write::write_all(&mut options.open(dir.join(name))?, bytes)
}

/// Write `bytes` to `path` through a file beside it, readable by the user
/// alone from its first byte, and renamed into place so that a reader
/// never sees half of it.
fn write_private(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let partial = write_partial(dir, path, bytes)?;
    fs::rename(&partial, path)
}

/// A new file beside `path` holding `bytes`, named so that two writers
/// never share one. On Windows the directory is
/// under the user's profile, which only the user can read.
fn write_partial(dir: &Path, path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    create_private_dir(dir)?;
    // The process, a count within it (two threads can share a moment), and
    // the moment (a process id comes back after a restart).
    static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let partial = path.with_extension(format!("{}-{count}-{stamp}.partial", std::process::id()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&partial)?;
    io::Write::write_all(&mut file, bytes)?;
    file.sync_all()?;
    Ok(partial)
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
        {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote");
        let first = machine_key(&remote).unwrap();
        let again = machine_key(&remote).unwrap();
        assert_eq!(first.public(), again.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&remote.join(KEY_FILE)), 0o600);
            assert_eq!(mode(&remote), 0o700);
        }
    }

    #[test]
    fn a_missing_key_with_phones_paired_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let first = machine_key(dir.path()).unwrap();
        let phone = DeviceKey::generate().public();
        add_device(
            dir.path(),
            Device::new(RoomId::random(), "Pixel", &phone, 0),
        )
        .unwrap();
        fs::remove_file(dir.path().join(KEY_FILE)).unwrap();
        assert!(machine_key(dir.path()).is_err());
        drop(first);
    }

    #[test]
    fn changes_made_at_once_all_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let first = Device::new(
            RoomId::random(),
            "Pixel",
            &DeviceKey::generate().public(),
            0,
        );
        add_device(&path, first.clone()).unwrap();
        let mut threads: Vec<_> = (0..8)
            .map(|n| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let phone = DeviceKey::generate().public();
                    add_device(
                        &path,
                        Device::new(RoomId::random(), &format!("p{n}"), &phone, 0),
                    )
                    .unwrap();
                })
            })
            .collect();
        let room = first.room.clone();
        let toggler = path.clone();
        threads.push(std::thread::spawn(move || {
            set_reply(&toggler, &room, false).unwrap();
        }));
        for thread in threads {
            thread.join().unwrap();
        }
        let all = devices(&path).unwrap();
        assert_eq!(all.len(), 9);
        // The right taken away stays taken away, whatever was paired meanwhile.
        assert!(!all.iter().find(|d| d.room == first.room).unwrap().reply);
    }

    #[test]
    fn keys_made_at_once_are_one_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || machine_key(&path).unwrap().public())
            })
            .collect();
        let keys: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(keys.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn pairing_a_phone_again_replaces_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let phone = DeviceKey::generate().public();
        add_device(
            dir.path(),
            Device::new(RoomId::random(), "Pixel", &phone, 1),
        )
        .unwrap();
        let again = Device::new(RoomId::random(), "Pixel", &phone, 2);
        add_device(dir.path(), again.clone()).unwrap();
        assert_eq!(devices(dir.path()).unwrap(), vec![again]);
    }

    #[test]
    fn devices_survive_the_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(devices(dir.path()).unwrap(), Vec::new());
        let phone = DeviceKey::generate().public();
        let room = RoomId::random();
        let device = Device::new(room, "Pixel", &phone, 1_800_000_000);
        save_devices(dir.path(), std::slice::from_ref(&device)).unwrap();
        let read = devices(dir.path()).unwrap();
        assert_eq!(read, vec![device]);
        assert_eq!(read[0].room_id(), Some(room));
        assert_eq!(read[0].public_key(), Some(phone));
        assert!(read[0].reply);
    }
}
