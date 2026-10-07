//! Lectura del visor: un worker lazy, un pedido pendiente y un resultado.
//! Los cambios de generación cancelan trabajo entre llamadas de lectura; no
//! interrumpen una llamada del sistema detenida en un filesystem remoto.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use super::file_viewer_document::SourceDocument;

pub(super) const MAX_VIEW_BYTES: u64 = 2 * 1024 * 1024;
pub(super) const MAX_VIEW_LINES: usize = 100_000;
const READ_CHUNK_BYTES: usize = 32 * 1024;

pub(super) enum FileContents {
    Text {
        /// Fuente y rangos completos preparados en el reader, sin copiar cada
        /// línea ni reconstruir el texto/los segmentos en el hilo de UI.
        document: Arc<SourceDocument>,
        truncated: bool,
        language: Option<String>,
    },
    Binary,
    Unreadable,
}

pub(super) struct FileReadResult {
    pub(super) token: u64,
    pub(super) path: PathBuf,
    pub(super) contents: FileContents,
}

struct FileReadJob {
    token: u64,
    path: PathBuf,
    notify: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
struct ReaderState {
    pending: Option<FileReadJob>,
    completed: Option<FileReadResult>,
}

struct ReaderShared {
    state: Mutex<ReaderState>,
    ready: Condvar,
    latest_token: AtomicU64,
    closed: AtomicBool,
}

impl ReaderShared {
    fn cancelled(&self, token: u64) -> bool {
        self.closed.load(Ordering::Acquire) || self.latest_token.load(Ordering::Acquire) != token
    }
}

struct ReaderExitGuard(Arc<ReaderShared>);

impl Drop for ReaderExitGuard {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.0.closed.store(true, Ordering::Release);
        let pending = state.pending.take();
        drop(state);
        self.0.ready.notify_all();
        drop(pending);
    }
}

pub(super) struct FileViewerReader {
    shared: Arc<ReaderShared>,
    next_token: u64,
}

impl FileViewerReader {
    pub(super) fn new(
        detect_language: impl Fn(&str, &str) -> Option<String> + Send + 'static,
    ) -> Self {
        Self::with_loader(move |path, cancelled| {
            load_file_for_view(path, cancelled, &detect_language)
        })
    }

    fn with_loader<F>(load: F) -> Self
    where
        F: Fn(&Path, &dyn Fn() -> bool) -> Option<FileContents> + Send + 'static,
    {
        let shared = Arc::new(ReaderShared {
            state: Mutex::new(ReaderState::default()),
            ready: Condvar::new(),
            latest_token: AtomicU64::new(0),
            closed: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("file-viewer-reader".to_owned())
            .spawn(move || {
                let _exit = ReaderExitGuard(Arc::clone(&worker));
                loop {
                    let mut state = worker
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    while state.pending.is_none() && !worker.closed.load(Ordering::Acquire) {
                        state = worker
                            .ready
                            .wait(state)
                            .unwrap_or_else(|error| error.into_inner());
                    }
                    if worker.closed.load(Ordering::Acquire) {
                        return;
                    }
                    let job = state.pending.take().expect("pending file read");
                    drop(state);

                    let cancelled = || worker.cancelled(job.token);
                    if cancelled() {
                        continue;
                    }
                    let Some(contents) = load(&job.path, &cancelled) else {
                        continue;
                    };
                    let mut state = worker
                        .state
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if cancelled() {
                        continue;
                    }
                    let replaced = state.completed.replace(FileReadResult {
                        token: job.token,
                        path: job.path,
                        contents,
                    });
                    drop(state);
                    drop(replaced);
                    (job.notify)();
                }
            })
            .is_ok();
        if !spawned {
            shared.closed.store(true, Ordering::Release);
        }
        Self {
            shared,
            next_token: 0,
        }
    }

    pub(super) fn request(&mut self, path: PathBuf, notify: impl FnOnce() + Send + 'static) -> u64 {
        self.next_token = self.next_token.wrapping_add(1);
        let token = self.next_token;
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared.latest_token.store(token, Ordering::Release);
        let pending = state.pending.take();
        let completed = state.completed.take();
        if !self.shared.closed.load(Ordering::Acquire) {
            state.pending = Some(FileReadJob {
                token,
                path,
                notify: Box::new(notify),
            });
        }
        drop(state);
        self.shared.ready.notify_one();
        drop(pending);
        drop(completed);
        token
    }

    pub(super) fn is_available(&self) -> bool {
        !self.shared.closed.load(Ordering::Acquire)
    }

    pub(super) fn is_current(&self, token: u64) -> bool {
        self.shared.latest_token.load(Ordering::Acquire) == token
    }

    pub(super) fn poll(&mut self) -> Option<FileReadResult> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .completed
            .take()
            .filter(|result| self.is_current(result.token))
    }

    pub(super) fn cancel(&mut self) {
        self.next_token = self.next_token.wrapping_add(1);
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared
            .latest_token
            .store(self.next_token, Ordering::Release);
        let pending = state.pending.take();
        let completed = state.completed.take();
        drop(state);
        drop(pending);
        drop(completed);
    }
}

impl Drop for FileViewerReader {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.shared.closed.store(true, Ordering::Release);
        let pending = state.pending.take();
        let completed = state.completed.take();
        drop(state);
        self.shared.ready.notify_all();
        // No hacer join: una lectura remota ya activa puede seguir esperando
        // al sistema operativo, pero no genera otro hilo por cada apertura.
        drop(pending);
        drop(completed);
    }
}

fn open_regular_file(path: &Path) -> io::Result<std::fs::File> {
    // Evita abrir dispositivos cuando ya se sabe que no son archivos. La
    // segunda comprobación sobre el handle cubre reemplazos entre stat/open.
    if !std::fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the file viewer only reads regular files",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Si el destino cambia a FIFO antes de open, no esperar a un writer;
        // tampoco adquirir un terminal de control al abrir un dispositivo.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() || !is_disk_file(&file) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the file viewer only reads regular files",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn is_disk_file(file: &std::fs::File) -> bool {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileType(handle: *mut std::ffi::c_void) -> u32;
    }
    // SAFETY: File mantiene vivo su handle durante esta consulta. GetFileType
    // no conserva el handle ni escribe memoria; sólo aceptamos FILE_TYPE_DISK.
    unsafe { GetFileType(file.as_raw_handle()) == 1 }
}

#[cfg(not(windows))]
fn is_disk_file(_: &std::fs::File) -> bool {
    true
}

fn read_bounded(
    reader: &mut impl Read,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Option<(Vec<u8>, bool)>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    let limit = MAX_VIEW_BYTES as usize + 1;
    while bytes.len() < limit {
        if cancelled() {
            return Ok(None);
        }
        let wanted = chunk.len().min(limit - bytes.len());
        match reader.read(&mut chunk[..wanted]) {
            Ok(0) => break,
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    if cancelled() {
        return Ok(None);
    }
    let truncated = bytes.len() as u64 > MAX_VIEW_BYTES;
    if truncated {
        bytes.truncate(MAX_VIEW_BYTES as usize);
    }
    Ok(Some((bytes, truncated)))
}

pub(super) fn load_file_for_view(
    path: &Path,
    cancelled: &dyn Fn() -> bool,
    detect_language: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<FileContents> {
    if cancelled() {
        return None;
    }
    let mut file = match open_regular_file(path) {
        Ok(file) => file,
        Err(_) => return Some(FileContents::Unreadable),
    };
    let (bytes, truncated_bytes) = match read_bounded(&mut file, cancelled) {
        Ok(Some(read)) => read,
        Ok(None) => return None,
        Err(_) => return Some(FileContents::Unreadable),
    };
    if bytes.contains(&0) {
        return Some(FileContents::Binary);
    }
    let mut source = String::from_utf8_lossy(&bytes).into_owned();
    drop(bytes);
    let mut source_lines = source.split_inclusive('\n');
    let mut source_end = 0;
    for line in source_lines.by_ref().take(MAX_VIEW_LINES) {
        if cancelled() {
            return None;
        }
        source_end += line.len();
    }
    let truncated_lines = source_lines.next().is_some();
    if truncated_lines {
        source.truncate(source_end);
    }
    if cancelled() {
        return None;
    }
    let document = Arc::new(SourceDocument::prepare_cancellable(
        Arc::from(source),
        cancelled,
    )?);
    let language = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| {
            document
                .logical_line_text(0)
                .and_then(|first| detect_language(name, first))
        });
    if cancelled() {
        return None;
    }
    Some(FileContents::Text {
        document,
        truncated: truncated_bytes || truncated_lines,
        language,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

    struct Fixture {
        directory: PathBuf,
        path: PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let directory = std::env::temp_dir()
                .join(format!("tc-file-viewer-reader-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&directory).expect("unique fixture directory");
            Self {
                path: directory.join(name),
                directory,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    fn load(path: &Path) -> FileContents {
        load_file_for_view(path, &|| false, &|_, _| None).expect("not cancelled")
    }

    fn text_contents(text: &str) -> FileContents {
        FileContents::Text {
            document: Arc::new(SourceDocument::prepare(Arc::from(text))),
            truncated: false,
            language: None,
        }
    }

    fn assert_source(contents: FileContents, expected: &str, expected_truncated: bool) {
        let FileContents::Text {
            document,
            truncated,
            ..
        } = contents
        else {
            panic!("expected text");
        };
        assert_eq!(document.source(), expected);
        assert_eq!(
            (0..document.logical_lines().len())
                .map(|index| document.logical_line_text(index).unwrap())
                .collect::<Vec<_>>(),
            expected.lines().collect::<Vec<_>>()
        );
        assert_eq!(truncated, expected_truncated);
    }

    #[test]
    fn source_keeps_crlf_unicode_empty_lines_and_the_original_eof() {
        let fixture = Fixture::new("código.js");
        for source in [
            "",
            "\n",
            "\r\n",
            "const título = \"café 🐈\";\r\n\r\nconst final = 3;",
            "uno\ndos\r\ntres\r",
        ] {
            std::fs::write(&fixture.path, source).expect("write source");
            assert_source(load(&fixture.path), source, false);
        }
    }

    #[test]
    fn the_exact_byte_cap_and_one_extra_byte_have_distinct_results() {
        let fixture = Fixture::new("large.txt");
        let source = "a".repeat(MAX_VIEW_BYTES as usize);
        std::fs::write(&fixture.path, &source).expect("write cap-sized file");
        assert_source(load(&fixture.path), &source, false);
        std::fs::write(&fixture.path, format!("{source}b")).expect("write oversized file");
        assert_source(load(&fixture.path), &source, true);
    }

    #[test]
    fn the_line_cap_limits_display_and_retained_source_together() {
        let fixture = Fixture::new("many-lines.txt");
        let expected = "x\r\n".repeat(MAX_VIEW_LINES);
        std::fs::write(&fixture.path, format!("{expected}extra\n")).expect("write many lines");
        assert_source(load(&fixture.path), &expected, true);
    }

    #[test]
    fn a_byte_cap_inside_utf8_keeps_the_existing_lossy_fallback_and_safe_fragments() {
        use super::super::file_viewer_document::FRAGMENT_BYTE_CAP;

        let fixture = Fixture::new("utf8-cap.txt");
        let prefix = "x".repeat(MAX_VIEW_BYTES as usize - 1);
        std::fs::write(&fixture.path, format!("{prefix}🙂")).expect("write split UTF-8");
        let FileContents::Text {
            document,
            truncated,
            ..
        } = load(&fixture.path)
        else {
            panic!("expected text");
        };
        assert!(truncated);
        assert_eq!(document.source(), format!("{prefix}�"));
        assert!(document.has_long_lines());
        assert!(document
            .fragments()
            .iter()
            .all(|fragment| fragment.source.len() <= FRAGMENT_BYTE_CAP));
        for fragment in document.fragments() {
            assert!(document.source().is_char_boundary(fragment.source.start));
            assert!(document.source().is_char_boundary(fragment.source.end));
        }
    }

    #[test]
    fn binary_missing_and_directory_paths_preserve_their_fallbacks() {
        let fixture = Fixture::new("binary.dat");
        assert!(matches!(load(&fixture.path), FileContents::Unreadable));
        assert!(matches!(load(&fixture.directory), FileContents::Unreadable));
        std::fs::write(&fixture.path, b"ELF\0data").expect("write binary");
        assert!(matches!(load(&fixture.path), FileContents::Binary));
    }

    #[test]
    fn language_detection_uses_the_original_file_name_and_visible_first_line() {
        let fixture = Fixture::new("código.js");
        std::fs::write(&fixture.path, "#!/bin/bash\r\necho hi\n").expect("write source");
        let contents = load_file_for_view(&fixture.path, &|| false, &|name, first| {
            assert_eq!(name, "código.js");
            assert_eq!(first, "#!/bin/bash");
            Some("detected".to_owned())
        })
        .expect("not cancelled");
        let FileContents::Text { language, .. } = contents else {
            panic!("expected text");
        };
        assert_eq!(language.as_deref(), Some("detected"));
    }

    #[test]
    fn cancelled_reads_stop_between_chunks() {
        struct CountedReader(Arc<AtomicU64>);
        impl Read for CountedReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.0.fetch_add(1, Ordering::Relaxed);
                buffer.fill(b'x');
                Ok(buffer.len())
            }
        }
        let calls = Arc::new(AtomicU64::new(0));
        let mut reader = CountedReader(Arc::clone(&calls));
        let cancelled = || calls.load(Ordering::Relaxed) >= 1;
        assert!(read_bounded(&mut reader, &cancelled).unwrap().is_none());
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn an_already_cancelled_request_does_not_open_or_detect_a_file() {
        let fixture = Fixture::new("absent.rs");
        assert!(load_file_for_view(&fixture.path, &|| true, &|_, _| {
            panic!("cancelled work must not detect a language")
        })
        .is_none());
    }

    #[test]
    fn the_real_worker_publishes_original_source_before_notifying() {
        let fixture = Fixture::new("script.js");
        let source = "// comentario\r\nconst título = \"café 🐈\";";
        std::fs::write(&fixture.path, source).expect("write source");
        let mut reader = FileViewerReader::new(|name, first| {
            assert_eq!(name, "script.js");
            assert_eq!(first, "// comentario");
            assert_eq!(std::thread::current().name(), Some("file-viewer-reader"));
            Some("JavaScript".to_owned())
        });
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let token = reader.request(fixture.path.clone(), move || {
            let _ = ready_tx.send(());
        });
        ready_rx.recv_timeout(DEADLINE).expect("read ready");
        let result = reader.poll().expect("result published before notification");
        assert_eq!(result.token, token);
        assert_eq!(result.path, fixture.path);
        assert_source(result.contents, source, false);
    }

    #[test]
    fn the_worker_prepares_complete_unicode_fragments_before_notifying() {
        use super::super::file_viewer_document::FRAGMENT_BYTE_CAP;

        let fixture = Fixture::new("long.js");
        let first_line = "é🙂e\u{301}".repeat(2400);
        let source = format!("{first_line}\r\nfin\n");
        std::fs::write(&fixture.path, &source).expect("write long source");
        let mut reader = FileViewerReader::new(move |name, first| {
            assert_eq!(name, "long.js");
            assert_eq!(
                first, first_line,
                "detection receives original logical text"
            );
            assert_eq!(std::thread::current().name(), Some("file-viewer-reader"));
            None
        });
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let token = reader.request(fixture.path.clone(), move || {
            let _ = ready_tx.send(());
        });
        ready_rx
            .recv_timeout(DEADLINE)
            .expect("fragmented document ready");
        let result = reader
            .poll()
            .expect("document published before notification");
        assert_eq!(result.token, token);
        let FileContents::Text {
            document,
            truncated,
            language,
        } = result.contents
        else {
            panic!("expected text");
        };
        assert!(!truncated);
        assert!(language.is_none());
        assert_eq!(document.source(), source);
        assert_eq!(document.logical_lines().len(), 2);
        assert_eq!(document.logical_line_text(1), Some("fin"));
        assert!(document.has_long_lines());
        assert!(document.fragments().len() > document.logical_lines().len());
        assert!(document
            .fragments()
            .iter()
            .all(|fragment| fragment.source.len() <= FRAGMENT_BYTE_CAP));
        assert!(Arc::ptr_eq(&document.source_arc(), &document.source_arc()));
        assert_source(
            FileContents::Text {
                document,
                truncated,
                language,
            },
            &source,
            false,
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_without_a_writer_and_character_devices_are_not_read() {
        use std::os::unix::ffi::OsStrExt;
        let fixture = Fixture::new("pipe");
        let path = std::ffi::CString::new(fixture.path.as_os_str().as_bytes()).unwrap();
        // SAFETY: path es una CString viva y el único destino está dentro del
        // directorio temporal exclusivo de este fixture; mode es válido.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(matches!(load(&fixture.path), FileContents::Unreadable));
        assert!(matches!(
            load(Path::new("/dev/null")),
            FileContents::Unreadable
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_to_regular_files_remain_readable() {
        let fixture = Fixture::new("source.txt");
        std::fs::write(&fixture.path, "hello\n").expect("write source");
        let link = fixture.directory.join("link.txt");
        std::os::unix::fs::symlink(&fixture.path, &link).expect("create owned link");
        assert_source(load(&link), "hello\n", false);
    }

    #[cfg(windows)]
    #[test]
    fn windows_character_devices_are_not_disk_files() {
        let file = std::fs::File::open("NUL").expect("open Windows NUL device");
        assert!(!is_disk_file(&file));
        assert!(matches!(load(Path::new("NUL")), FileContents::Unreadable));
    }

    #[test]
    fn same_path_reopens_replace_queued_jobs_and_cancel_the_active_generation() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let calls = Arc::new(AtomicU64::new(0));
        let worker_calls = Arc::clone(&calls);
        let mut reader = FileViewerReader::with_loader(move |path, cancelled| {
            assert_eq!(path, Path::new("same.rs"));
            let call = worker_calls.fetch_add(1, Ordering::Relaxed);
            if call == 0 {
                let _ = entered_tx.send(());
                release_rx.recv().expect("release first generation");
                assert!(cancelled());
                return None;
            }
            Some(text_contents("latest"))
        });
        let notifications = Arc::new(AtomicU64::new(0));
        let first_notifications = Arc::clone(&notifications);
        let first = reader.request(PathBuf::from("same.rs"), move || {
            first_notifications.fetch_add(1, Ordering::Relaxed);
        });
        entered_rx.recv_timeout(DEADLINE).expect("first active");
        for _ in 0..128 {
            let obsolete_notifications = Arc::clone(&notifications);
            reader.request(PathBuf::from("same.rs"), move || {
                obsolete_notifications.fetch_add(1, Ordering::Relaxed);
            });
        }
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let latest = reader.request(PathBuf::from("same.rs"), move || {
            let _ = ready_tx.send(());
        });
        assert_ne!(first, latest);
        assert!(!reader.is_current(first));
        {
            let state = reader.shared.state.lock().unwrap();
            assert_eq!(state.pending.as_ref().unwrap().token, latest);
            assert!(state.completed.is_none());
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        release_tx.send(()).expect("release obsolete generation");
        ready_rx.recv_timeout(DEADLINE).expect("latest ready");
        let result = reader.poll().expect("latest result published");
        assert_eq!(result.token, latest);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(notifications.load(Ordering::Relaxed), 0);
        assert!(reader.poll().is_none());
    }

    #[test]
    fn a_new_generation_discards_the_previous_unpolled_result() {
        let mut reader = FileViewerReader::with_loader(|_, _| Some(text_contents("loaded")));
        let (first_tx, first_rx) = std::sync::mpsc::channel();
        let first = reader.request(PathBuf::from("same.rs"), move || {
            let _ = first_tx.send(());
        });
        first_rx.recv_timeout(DEADLINE).expect("first ready");
        assert_eq!(
            reader
                .shared
                .state
                .lock()
                .unwrap()
                .completed
                .as_ref()
                .unwrap()
                .token,
            first
        );
        let (latest_tx, latest_rx) = std::sync::mpsc::channel();
        let latest = reader.request(PathBuf::from("same.rs"), move || {
            let _ = latest_tx.send(());
        });
        latest_rx.recv_timeout(DEADLINE).expect("latest ready");
        let result = reader.poll().expect("latest result");
        assert_eq!(result.token, latest);
        assert!(reader.poll().is_none());
    }

    #[test]
    fn cancellation_clears_the_slots_and_the_reader_can_be_reused() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let mut reader = FileViewerReader::with_loader(move |path, cancelled| {
            if path == Path::new("blocked") {
                let _ = entered_tx.send(());
                release_rx.recv().expect("release cancelled read");
                let _ = cancelled_tx.send(cancelled());
                return None;
            }
            Some(text_contents("usable"))
        });
        reader.request(PathBuf::from("blocked"), || {});
        entered_rx.recv_timeout(DEADLINE).expect("active read");
        reader.request(PathBuf::from("obsolete"), || {});
        reader.cancel();
        {
            let state = reader.shared.state.lock().unwrap();
            assert!(state.pending.is_none());
            assert!(state.completed.is_none());
        }
        release_tx.send(()).expect("release cancelled read");
        assert!(cancelled_rx
            .recv_timeout(DEADLINE)
            .expect("observed cancellation"));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let token = reader.request(PathBuf::from("new"), move || {
            let _ = ready_tx.send(());
        });
        ready_rx.recv_timeout(DEADLINE).expect("new ready");
        assert_eq!(reader.poll().unwrap().token, token);
    }

    #[test]
    fn drop_cancels_without_waiting_for_an_active_read() {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (cancelled_tx, cancelled_rx) = std::sync::mpsc::channel();
        let mut reader = FileViewerReader::with_loader(move |_, cancelled| {
            let _ = entered_tx.send(());
            release_rx.recv().expect("release active read");
            let _ = cancelled_tx.send(cancelled());
            None
        });
        reader.request(PathBuf::from("blocked"), || {});
        entered_rx.recv_timeout(DEADLINE).expect("active read");
        let (dropped_tx, dropped_rx) = std::sync::mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(reader);
            let _ = dropped_tx.send(());
        });
        let dropped_without_release = dropped_rx.recv_timeout(DEADLINE);
        let _ = release_tx.send(());
        dropper.join().expect("drop thread");
        dropped_without_release.expect("Drop must not join an active read");
        assert!(cancelled_rx.recv_timeout(DEADLINE).expect("observed Drop"));
    }
}
