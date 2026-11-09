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
/// The one-time changes to these files that have run, by name.
const MIGRATIONS_FILE: &str = "migrations.json";

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
///
/// A key revoked by a migration is refused: it is a phone's key from before
/// the page moved, which may still sit in storage the page no longer
/// controls.
pub(crate) fn add_device(dir: &Path, mut device: Device) -> io::Result<()> {
    let migrations = migrated(dir)?;
    // A phone paired now is never older than the revocation, even with a
    // clock set back since: see `Migrations::refuses`.
    if let Some(revocation) = &migrations.revoke_pairings_before_page_move {
        device.paired_at_unix = device.paired_at_unix.max(revocation.at_unix);
    }
    if migrations.refuses(&device) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "this phone's key was revoked when the phone page moved; open app.splitlane.dev on \
             the phone and pair it from there",
        ));
    }
    edit_devices(dir, |all| {
        all.retain(|kept| kept.public != device.public);
        all.push(device);
    })?;
    // Pairing again is what the note about the revoked phones asks for. The
    // phone is paired whatever happens here: a note left up is no reason to
    // report a pairing that worked as failed.
    if let Err(e) = dismiss_revocation_notice(dir) {
        log::warn!("paired, but the note about revoked phones stays: {e}");
    }
    Ok(())
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
///
/// Every reader of the phones comes through here - the window, the session
/// host, the bridge before each request, `splitlane remote` - so this is
/// where the migrations run: whichever of them starts first on a new
/// version runs them, and none of them ever sees a phone a migration
/// revoked, even one put back in the file by hand or from a backup.
pub(crate) fn devices(dir: &Path) -> io::Result<Vec<Device>> {
    let migrations = migrated(dir)?;
    let mut all = read_devices(dir)?;
    all.retain(|device| !migrations.refuses(device));
    Ok(all)
}

fn read_devices(dir: &Path) -> io::Result<Vec<Device>> {
    match fs::read(dir.join(DEVICES_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// The one-time changes made to the files here, each recorded under its
/// own name once it has run, so that it never runs again. A later version
/// adds its migrations beside these.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Migrations {
    /// Records this version does not know, written back as they were read:
    /// an older build that rewrites the file (dismissing the note) must not
    /// drop a newer one's record, or that migration would run again.
    #[serde(flatten)]
    others: serde_json::Map<String, serde_json::Value>,
    /// The phone's page moved from splitlane.dev/app to app.splitlane.dev.
    /// A browser keeps a page's storage per origin, so every phone paired
    /// before had to pair again anyway; its old key stays behind in
    /// splitlane.dev's storage, which on Android Chrome shares with the
    /// browser and so with whatever script the site there runs. Revoking
    /// those keys makes what is left behind open nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revoke_pairings_before_page_move: Option<Revocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Revocation {
    /// The Splitlane version that ran it, for whoever reads the file.
    by_version: String,
    at_unix: u64,
    revoked: Vec<RevokedPhone>,
    /// Settings -> Phone has said why the phones are gone: the person
    /// dismissed the note, or paired a phone again.
    #[serde(default)]
    noticed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RevokedPhone {
    pub(crate) name: String,
    /// The phone's public key, as in `Device::public`.
    pub(crate) public: String,
}

impl Migrations {
    /// Whether `device` is one this computer must not answer: a key the
    /// revocation took, or a record paired before it ran. The second covers
    /// a `devices.json` put back from a backup, or one that was not where
    /// the revocation looked: whatever was paired before the page moved is
    /// refused, whether or not its key was seen. Every phone this version
    /// pairs is dated no earlier than the revocation (`add_device`).
    fn refuses(&self, device: &Device) -> bool {
        self.is_revoked(&device.public)
            || self
                .revoke_pairings_before_page_move
                .as_ref()
                .is_some_and(|revocation| device.paired_at_unix < revocation.at_unix)
    }

    fn is_revoked(&self, public: &str) -> bool {
        self.revoke_pairings_before_page_move
            .as_ref()
            .is_some_and(|revocation| {
                revocation
                    .revoked
                    .iter()
                    .any(|phone| phone.public == public)
            })
    }
}

/// One migration at a time within this process; see [`migrated`].
static MIGRATING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The migrations, run first if they have not been.
///
/// Before anything is paired the directory may not exist; then there is
/// nothing to revoke and nothing is written. Once it exists the record is
/// written before the first phone is added (`add_device` comes through
/// here), so a phone paired by this version is never taken for one paired
/// before it.
fn migrated(dir: &Path) -> io::Result<Migrations> {
    let _one_at_a_time = MIGRATING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let migrations = read_migrations(dir)?;
    if migrations.revoke_pairings_before_page_move.is_some() || !dir.is_dir() {
        return Ok(migrations);
    }
    let before = read_devices(dir)?;
    revoke(dir, migrations, &before)
}

/// Revoke the phones in `before`, as read just now, unless another process
/// got there first.
///
/// The window, the session host, the bridge and `splitlane remote` are
/// separate processes and may all start at once on the first run of this
/// version. Only one of them records the revocation: the record is put in
/// place with a link that fails when the file exists, as the machine's key
/// is. Another that read `devices.json` earlier - perhaps before a phone was
/// paired by the one that won - takes the winner's record and touches
/// nothing.
///
/// The record of the revoked keys is in place before the phones are taken
/// out of `devices.json`, so a stop between the two leaves them refused by
/// [`devices`] rather than served. And only those keys are taken out, read
/// again at that moment: a phone added meanwhile stays.
fn revoke(dir: &Path, mut migrations: Migrations, before: &[Device]) -> io::Result<Migrations> {
    let path = dir.join(MIGRATIONS_FILE);
    if path.exists() {
        let current = read_migrations(dir)?;
        if current.revoke_pairings_before_page_move.is_some() {
            return Ok(current);
        }
        migrations = current;
    }
    migrations.revoke_pairings_before_page_move = Some(Revocation {
        by_version: env!("CARGO_PKG_VERSION").to_string(),
        at_unix: super::now_unix(),
        revoked: before
            .iter()
            .map(|device| RevokedPhone {
                name: device.name.clone(),
                public: device.public.clone(),
            })
            .collect(),
        noticed: before.is_empty(),
    });
    if path.exists() {
        // A record without this migration in it, read just now: one this
        // version did not write, so no other process of it is writing one.
        write_migrations(dir, &migrations)?;
    } else {
        let bytes = serde_json::to_vec_pretty(&migrations).map_err(io::Error::other)?;
        let partial = write_partial(dir, &path, &bytes)?;
        let placed = fs::hard_link(&partial, &path);
        let _ = fs::remove_file(&partial);
        match placed {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return read_migrations(dir),
            Err(e) => return Err(e),
        }
    }
    if !before.is_empty() {
        // The record's name on disk before devices.json changes: after a
        // power cut, the file could otherwise be found emptied with no
        // record of why.
        sync_dir(dir)?;
        let mut kept = read_devices(dir)?;
        kept.retain(|device| !migrations.is_revoked(&device.public));
        save_devices(dir, &kept)?;
        log::info!(
            "revoked {} phone(s) paired before the phone page moved to app.splitlane.dev",
            before.len()
        );
    }
    Ok(migrations)
}

fn read_migrations(dir: &Path) -> io::Result<Migrations> {
    match fs::read(dir.join(MIGRATIONS_FILE)) {
        // A record that does not parse is an error, not a reason to run the
        // migrations again: running one twice could revoke a phone paired
        // since.
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", dir.join(MIGRATIONS_FILE).display()),
            )
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Migrations::default()),
        Err(e) => Err(e),
    }
}

fn write_migrations(dir: &Path, migrations: &Migrations) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(migrations).map_err(io::Error::other)?;
    write_private(dir, &dir.join(MIGRATIONS_FILE), &bytes)
}

/// The phones revoked when the page moved, while Settings -> Phone still
/// has to say why they are gone; empty once it has, or if there were none.
pub(crate) fn revocation_notice(dir: &Path) -> io::Result<Vec<RevokedPhone>> {
    Ok(migrated(dir)?
        .revoke_pairings_before_page_move
        .filter(|revocation| !revocation.noticed)
        .map(|revocation| revocation.revoked)
        .unwrap_or_default())
}

/// The note about the revoked phones has been read; it is not shown again.
pub(crate) fn dismiss_revocation_notice(dir: &Path) -> io::Result<()> {
    migrated(dir)?;
    let _one_at_a_time = MIGRATING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut migrations = read_migrations(dir)?;
    match &mut migrations.revoke_pairings_before_page_move {
        Some(revocation) if !revocation.noticed => {
            revocation.noticed = true;
            write_migrations(dir, &migrations)
        }
        _ => Ok(()),
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

/// Make the names last written in `dir` durable. Windows has no handle on
/// a directory to flush; NTFS journals a rename itself.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
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

    fn now() -> u64 {
        super::super::now_unix()
    }

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
            Device::new(RoomId::random(), "Pixel", &phone, now()),
        )
        .unwrap();
        let again = Device::new(RoomId::random(), "Pixel", &phone, now() + 1);
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

    /// A phone as an older version left it: in `devices.json`, with no
    /// migration recorded.
    fn paired_before_the_move(dir: &Path, name: &str) -> Device {
        let device = Device::new(
            RoomId::random(),
            name,
            &DeviceKey::generate().public(),
            1_700_000_000,
        );
        let mut all = read_devices(dir).unwrap();
        all.push(device.clone());
        save_devices(dir, &all).unwrap();
        device
    }

    #[test]
    fn phones_paired_before_the_move_are_revoked_once() {
        let dir = tempfile::tempdir().unwrap();
        let old = paired_before_the_move(dir.path(), "Pixel");
        let other = paired_before_the_move(dir.path(), "iPhone");
        // The first start of this version: none of them is served.
        assert_eq!(devices(dir.path()).unwrap(), Vec::new());
        assert_eq!(read_devices(dir.path()).unwrap(), Vec::new());
        let notice = revocation_notice(dir.path()).unwrap();
        assert_eq!(
            notice.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["Pixel", "iPhone"]
        );
        assert_eq!(notice[0].public, old.public);
        assert_eq!(notice[1].public, other.public);
        // A phone paired since is kept on every later start.
        let new = Device::new(
            RoomId::random(),
            "Pixel",
            &DeviceKey::generate().public(),
            now(),
        );
        add_device(dir.path(), new.clone()).unwrap();
        for _ in 0..3 {
            assert_eq!(devices(dir.path()).unwrap(), vec![new.clone()]);
        }
    }

    #[test]
    fn a_revoked_key_is_refused_wherever_it_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let old = paired_before_the_move(dir.path(), "Pixel");
        assert_eq!(devices(dir.path()).unwrap(), Vec::new());
        // Pairing the same key again.
        let again = Device {
            room: RoomId::random().to_text(),
            ..old.clone()
        };
        let refused = add_device(dir.path(), again).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        // The record put back by hand, or by a backup: the bridge does not
        // find it, so it leaves the room and answers nothing.
        save_devices(dir.path(), std::slice::from_ref(&old)).unwrap();
        assert_eq!(devices(dir.path()).unwrap(), Vec::new());
        assert_eq!(device_paired_as(dir.path(), &old).unwrap(), None);
    }

    #[test]
    fn nothing_paired_writes_nothing_and_says_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote");
        assert_eq!(devices(&remote).unwrap(), Vec::new());
        assert!(
            !remote.exists(),
            "a computer that never paired gets no files"
        );
        // The first pairing of this version records the migration first, so
        // the phone is not taken for one from before the move.
        machine_key(&remote).unwrap();
        let phone = Device::new(
            RoomId::random(),
            "Pixel",
            &DeviceKey::generate().public(),
            now(),
        );
        add_device(&remote, phone.clone()).unwrap();
        assert!(remote.join(MIGRATIONS_FILE).exists());
        assert_eq!(devices(&remote).unwrap(), vec![phone]);
        assert_eq!(revocation_notice(&remote).unwrap(), Vec::new());
    }

    #[test]
    fn the_note_is_shown_until_dismissed_or_a_phone_is_paired() {
        let dir = tempfile::tempdir().unwrap();
        paired_before_the_move(dir.path(), "Pixel");
        assert_eq!(revocation_notice(dir.path()).unwrap().len(), 1);
        assert_eq!(revocation_notice(dir.path()).unwrap().len(), 1);
        dismiss_revocation_notice(dir.path()).unwrap();
        assert_eq!(revocation_notice(dir.path()).unwrap(), Vec::new());

        let dir = tempfile::tempdir().unwrap();
        paired_before_the_move(dir.path(), "Pixel");
        assert_eq!(revocation_notice(dir.path()).unwrap().len(), 1);
        let new = Device::new(
            RoomId::random(),
            "Pixel",
            &DeviceKey::generate().public(),
            now(),
        );
        add_device(dir.path(), new).unwrap();
        assert_eq!(revocation_notice(dir.path()).unwrap(), Vec::new());
    }

    #[test]
    fn a_record_that_does_not_parse_runs_nothing_again() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(devices(dir.path()).unwrap(), Vec::new());
        let phone = Device::new(
            RoomId::random(),
            "Pixel",
            &DeviceKey::generate().public(),
            now(),
        );
        add_device(dir.path(), phone.clone()).unwrap();
        fs::write(dir.path().join(MIGRATIONS_FILE), b"{not json").unwrap();
        assert!(devices(dir.path()).is_err());
        assert_eq!(read_devices(dir.path()).unwrap(), vec![phone]);
    }

    #[test]
    fn a_process_that_read_late_neither_revokes_nor_removes_a_new_phone() {
        let dir = tempfile::tempdir().unwrap();
        let old = paired_before_the_move(dir.path(), "Pixel");
        // One process reads the phones on its first start and stalls before
        // recording anything.
        let stale = read_devices(dir.path()).unwrap();
        assert_eq!(stale, vec![old.clone()]);
        // Another runs the migration, and a phone is paired through it.
        let new = Device::new(
            RoomId::random(),
            "Pixel",
            &DeviceKey::generate().public(),
            now(),
        );
        add_device(dir.path(), new.clone()).unwrap();
        // The first one resumes with what it read: it finds the record in
        // place and takes it.
        let taken = revoke(dir.path(), Migrations::default(), &[new.clone()]).unwrap();
        assert!(taken.is_revoked(&old.public));
        assert!(!taken.is_revoked(&new.public));
        let kept = devices(dir.path()).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].public, new.public);
    }

    #[test]
    fn a_record_from_before_the_move_is_refused_even_with_an_unseen_key() {
        let dir = tempfile::tempdir().unwrap();
        // The revocation runs with nothing paired.
        assert_eq!(devices(dir.path()).unwrap(), Vec::new());
        // Then a devices.json from before the move comes back, with a key
        // the revocation never saw.
        let restored = paired_before_the_move(dir.path(), "Pixel");
        assert_eq!(devices(dir.path()).unwrap(), Vec::new());
        assert_eq!(device_paired_as(dir.path(), &restored).unwrap(), None);
        // A phone paired now, even dated earlier by a clock set back, stays.
        let early = Device::new(
            RoomId::random(),
            "iPhone",
            &DeviceKey::generate().public(),
            5,
        );
        add_device(dir.path(), early.clone()).unwrap();
        let kept = devices(dir.path()).unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].public, early.public);
        assert!(kept[0].paired_at_unix >= now() - 60);
    }

    #[test]
    fn a_later_versions_records_survive_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        paired_before_the_move(dir.path(), "Pixel");
        assert_eq!(devices(dir.path()).unwrap(), Vec::new());
        let path = dir.path().join(MIGRATIONS_FILE);
        let mut written: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        written["a_later_migration"] = serde_json::json!({ "at_unix": 1 });
        fs::write(&path, serde_json::to_vec(&written).unwrap()).unwrap();
        dismiss_revocation_notice(dir.path()).unwrap();
        let after: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(after["a_later_migration"]["at_unix"], 1);
        assert_eq!(after["revoke_pairings_before_page_move"]["noticed"], true);
    }
}
