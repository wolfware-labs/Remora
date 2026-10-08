//! An in-process daemon on a temporary data directory, and a project folder.
#![allow(dead_code, clippy::unwrap_used)]

use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use interprocess::local_socket::Stream;
use interprocess::local_socket::traits::Stream as _;
use remora_daemon::{Daemon, DaemonConfig, DaemonHandle, ManualClock};
use remora_engine::RestorePoint;
use remora_ipc::auth::{self, Secret};
use remora_ipc::{
    Client, Outcome, Request, RequestEnvelope, Response, ResponseEnvelope, endpoint, read_frame, write_frame,
};
use tempfile::TempDir;

pub const START_MS: i64 = 1_700_000_000_000;

pub struct TestDaemon {
    pub data: TempDir,
    pub project: TempDir,
    pub clock: ManualClock,
    pub daemon: DaemonHandle,
}

impl TestDaemon {
    pub fn start() -> Self {
        let data = TempDir::new().unwrap();
        let clock = ManualClock::new(START_MS);
        let daemon = Daemon::start(config(data.path(), &clock)).unwrap();
        Self {
            data,
            project: TempDir::new().unwrap(),
            clock,
            daemon,
        }
    }

    pub fn root(&self) -> PathBuf {
        self.project.path().to_path_buf()
    }

    pub fn client(&self) -> Client {
        Client::connect(self.data.path()).unwrap()
    }

    /// A raw connection, for sending bytes that `Client` never would.
    pub fn raw(&self) -> BufReader<Stream> {
        let endpoint = endpoint(self.daemon.data_dir());
        BufReader::new(Stream::connect(endpoint.name().unwrap()).unwrap())
    }

    /// A raw connection that has completed the handshake with the daemon's secret.
    pub fn authenticated(&self) -> BufReader<Stream> {
        let mut conn = self.raw();
        let secret = Secret::load(self.data.path()).unwrap();
        assert!(handshake(&mut conn, &secret), "the daemon refused its own secret");
        conn
    }

    pub fn write(&self, rel: &str, contents: &str) {
        let path = self.project.path().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.project.path().join(rel)).unwrap()
    }

    pub fn begin(&self, client: &mut Client, session: &str, prompt: Option<&str>) -> Response {
        client
            .call(Request::BeginTurn {
                root: self.root(),
                session: session.into(),
                agent: "claude-code".into(),
                prompt: prompt.map(Into::into),
            })
            .unwrap()
    }

    pub fn before(&self, client: &mut Client, session: &str, command: &str) -> Response {
        client
            .call(Request::BeforeCommand {
                root: self.root(),
                session: session.into(),
                agent: "claude-code".into(),
                command: command.into(),
                tool_use_id: None,
            })
            .unwrap()
    }

    pub fn end(&self, client: &mut Client, session: &str) -> Response {
        client
            .call(Request::EndTurn {
                root: self.root(),
                session: session.into(),
                agent: "claude-code".into(),
            })
            .unwrap()
    }

    pub fn points(&self, client: &mut Client) -> Vec<RestorePoint> {
        let request = Request::RestorePoints {
            root: self.root(),
            skip: 0,
            limit: None,
        };
        match client.call(request).unwrap() {
            Response::RestorePoints { points, .. } => points,
            other => panic!("unexpected {other:?}"),
        }
    }

    pub fn status(&self, client: &mut Client) -> remora_engine::Status {
        match client.call(Request::Status { root: self.root() }).unwrap() {
            Response::Status { status } => status,
            other => panic!("unexpected {other:?}"),
        }
    }

    pub fn stop(&self) {
        self.daemon.shutdown();
        self.daemon.wait();
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn config(data_dir: &Path, clock: &ManualClock) -> DaemonConfig {
    DaemonConfig {
        clock: Arc::new(clock.clone()),
        // Sweeps run when a test calls `sweep()`, never on their own.
        sweep_every: Duration::from_secs(3_600),
        ..DaemonConfig::new(data_dir)
    }
}

/// Send `request` on a raw connection and read the answer (`None` once the daemon closed it).
pub fn exchange(conn: &mut BufReader<Stream>, id: u64, req: Request) -> Option<ResponseEnvelope> {
    write_frame(conn.get_mut(), &RequestEnvelope { v: 1, id, req }).ok()?;
    let line = read_frame(conn).ok()??;
    Some(serde_json::from_str(&line).unwrap())
}

/// Run the handshake on a raw connection with `secret`; true if the daemon accepted it.
pub fn handshake(conn: &mut BufReader<Stream>, secret: &Secret) -> bool {
    let nonce = auth::nonce().unwrap();
    let hello = Request::Hello {
        client_version: "test".into(),
        protocol: 1,
        nonce: nonce.clone(),
    };
    let Some(ResponseEnvelope {
        outcome: Outcome::Ok(Response::Hello {
            nonce: server_nonce, ..
        }),
        ..
    }) = exchange(conn, 1, hello)
    else {
        return false;
    };
    let prove = Request::Prove {
        proof: secret.client_proof(&server_nonce),
    };
    matches!(
        exchange(conn, 2, prove),
        Some(ResponseEnvelope {
            outcome: Outcome::Ok(Response::Authenticated),
            ..
        })
    )
}
