use std::{
    io,
    os::fd::{AsRawFd, OwnedFd},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use nix::{
    fcntl::{fcntl, FcntlArg, OFlag},
    pty::openpty,
    sys::termios::{cfmakeraw, tcgetattr, tcsetattr, SetArg},
    unistd::{read, ttyname, write},
};
use tokio::io::unix::AsyncFd;

/// A local pseudo-terminal standing in for the remote serial port.
pub struct Pty {
    master: AsyncFd<OwnedFd>,
    /// Held open for the lifetime of the session. If the last slave fd closes,
    /// reads on the master return EIO, so dropping this would tear down the
    /// link every time an application closes the port between runs.
    _slave: OwnedFd,
    pub path: PathBuf,
    link: Option<PathBuf>,
}

impl Pty {
    pub fn open() -> Result<Self> {
        let pty = openpty(None, None).context("openpty")?;

        let mut tio = tcgetattr(&pty.slave).context("tcgetattr on pty slave")?;
        cfmakeraw(&mut tio);
        tcsetattr(&pty.slave, SetArg::TCSANOW, &tio).context("tcsetattr on pty slave")?;

        let path = ttyname(&pty.slave).context("ttyname of pty slave")?;

        let flags = OFlag::from_bits_truncate(fcntl(pty.master.as_raw_fd(), FcntlArg::F_GETFL)?);
        fcntl(
            pty.master.as_raw_fd(),
            FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK),
        )
        .context("setting pty master non-blocking")?;

        Ok(Self {
            master: AsyncFd::new(pty.master)?,
            _slave: pty.slave,
            path,
            link: None,
        })
    }

    /// Point a stable, human-chosen path at this session's pty.
    ///
    /// The pty name itself is allocated by the kernel and changes on every
    /// reconnect, so configuration elsewhere should reference the symlink.
    pub fn link_at(&mut self, link: &Path) -> Result<()> {
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        match std::fs::symlink_metadata(link) {
            Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(link)?,
            Ok(_) => anyhow::bail!("{} exists and is not a symlink", link.display()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("checking link path"),
        }
        std::os::unix::fs::symlink(&self.path, link)
            .with_context(|| format!("linking {} -> {}", link.display(), self.path.display()))?;
        self.link = Some(link.to_path_buf());
        Ok(())
    }

    pub async fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let mut guard = self.master.readable().await?;
            match guard.try_io(|inner| read(inner.get_ref().as_raw_fd(), buf).map_err(Into::into)) {
                Ok(result) => return result,
                Err(_would_block) => continue,
            }
        }
    }

    pub async fn write_all(&self, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            let mut guard = self.master.writable().await?;
            match guard.try_io(|inner| write(inner.get_ref(), buf).map_err(Into::into)) {
                Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(Ok(n)) => buf = &buf[n..],
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Some(link) = &self.link {
            let _ = std::fs::remove_file(link);
        }
    }
}
