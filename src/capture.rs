use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct CaptureState {
    limit: usize,
    written: AtomicUsize,
    truncated: AtomicBool,
}

impl CaptureState {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            written: AtomicUsize::new(0),
            truncated: AtomicBool::new(false),
        }
    }

    pub fn truncated(&self) -> bool {
        self.truncated.load(Ordering::Acquire)
    }

    pub fn written(&self) -> usize {
        self.written.load(Ordering::Acquire)
    }
}

pub async fn capture<O: tokio::io::AsyncRead + Unpin, E: tokio::io::AsyncRead + Unpin>(
    mut stdout: O,
    mut stderr: E,
    mut stdout_file: tokio::fs::File,
    mut stderr_file: tokio::fs::File,
    state: Arc<CaptureState>,
) -> io::Result<()> {
    let mut out_buf = [0u8; 8192];
    let mut err_buf = [0u8; 8192];
    let mut out_done = false;
    let mut err_done = false;

    while !out_done || !err_done {
        // Select only cancellation-safe reads. Selecting read+write futures could
        // cancel a partially written chunk when the other stream becomes ready.
        let (is_stdout, n) = tokio::select! {
            n = stdout.read(&mut out_buf), if !out_done => (true, n?),
            n = stderr.read(&mut err_buf), if !err_done => (false, n?),
        };
        if n == 0 {
            if is_stdout {
                out_done = true;
            } else {
                err_done = true;
            }
            continue;
        }
        let written = state.written.load(Ordering::Acquire);
        let allowed = n.min(state.limit.saturating_sub(written));
        if is_stdout {
            stdout_file.write_all(&out_buf[..allowed]).await?;
        } else {
            stderr_file.write_all(&err_buf[..allowed]).await?;
        }
        state.written.store(written + allowed, Ordering::Release);
        if allowed < n {
            state.truncated.store(true, Ordering::Release);
            break;
        }
    }

    stdout_file.flush().await?;
    stderr_file.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    async fn open_rw(path: &std::path::Path) -> tokio::fs::File {
        tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn combined_writes_never_exceed_cap() {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out.log");
        let err_path = dir.path().join("err.log");
        let out_file = open_rw(&out_path).await;
        let err_file = open_rw(&err_path).await;

        let stdout = Cursor::new(vec![b'a'; 100]);
        let stderr = Cursor::new(vec![b'b'; 100]);
        let state = Arc::new(CaptureState::new(50));

        capture(stdout, stderr, out_file, err_file, state.clone())
            .await
            .unwrap();

        let out_meta = tokio::fs::metadata(&out_path).await.unwrap();
        let err_meta = tokio::fs::metadata(&err_path).await.unwrap();
        let total = out_meta.len() + err_meta.len();

        assert!(total <= 50);
        assert_eq!(state.written() as u64, total);
        assert!(state.truncated());
    }

    #[tokio::test]
    async fn exact_cap_with_clean_eof_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out.log");
        let err_path = dir.path().join("err.log");
        let out_file = open_rw(&out_path).await;
        let err_file = open_rw(&err_path).await;

        let out_data = vec![b'x'; 20];
        let err_data = vec![b'y'; 10];
        let stdout = Cursor::new(out_data.clone());
        let stderr = Cursor::new(err_data.clone());
        let state = Arc::new(CaptureState::new(30));

        capture(stdout, stderr, out_file, err_file, state.clone())
            .await
            .unwrap();

        let out_contents = tokio::fs::read(&out_path).await.unwrap();
        let err_contents = tokio::fs::read(&err_path).await.unwrap();

        assert_eq!(out_contents, out_data);
        assert_eq!(err_contents, err_data);
        assert_eq!(state.written(), 30);
        assert!(!state.truncated());
    }
}
