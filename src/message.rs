use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::*;
use tokio::sync::oneshot;

use quiche::Connection;

pub enum DtpMsg {
    DtpSend {
        buf: Vec<u8>,
        fin: bool,
        stream_id: u64,
        resp: Responder<DtpMsg>,
    },
    DtpSendRet {
        res: Result<usize, quiche::Error>,
    },
    DtpSendRetry,
    DtpRecv {
        stream_id: u64,
        buflen: usize,
        resp: Responder<DtpMsg>,
    },
    DtpRecvRet {
        res: Result<(usize, bool), quiche::Error>,
    },
    DtpClose {
        app: bool,
        err: u64,
        reason: String,
        resp: Responder<DtpMsg>,
    },
    DtpCloseRet {
        res: quiche::Result<()>,
    },
}

/// Provided by the requester and used by the manager task to send
/// the command response back to the requester.
type Responder<T> = oneshot::Sender<T>;

/// rx: 自己接收 api 发来的请求的 channel
/// conn: 用来和 loop 交互的 connection，可以用于收发数据
/// waker: 在收发完数据后需要 wake poll
pub async fn recv_msg_loop(
    mut rx: Receiver<DtpMsg>,
    conn: Arc<Mutex<Connection>>,
    waker: Arc<Mutex<mio::Waker>>,
) {
    // Start receiving messages
    while let Some(cmd) = rx.recv().await {
        match cmd {
            DtpMsg::DtpSend {
                buf,
                fin,
                stream_id,
                resp,
            } => {
                eprintln!("recv stream_id {}", stream_id);
                if conn.lock().unwrap().is_established() {
                    let res = conn
                        .lock()
                        .unwrap()
                        .stream_send(stream_id, buf.as_slice(), fin);
                    // Ignore errors
                    let _ = resp.send(DtpMsg::DtpSendRet { res });
                } else {
                    eprintln!("not established!");
                    let _ = resp.send(DtpMsg::DtpSendRetry);
                }
            }
            DtpMsg::DtpRecv {
                stream_id,
                buflen,
                resp,
            } => {
                let mut out = Vec::with_capacity(buflen as usize);

                let res = conn
                    .lock()
                    .unwrap()
                    .stream_recv(stream_id, &mut out.as_mut_slice());
                // Ignore errors
                let _ = resp.send(DtpMsg::DtpRecvRet { res });
            }
            DtpMsg::DtpClose {
                app,
                err,
                reason,
                resp,
            } => {
                let res = conn.lock().unwrap().close(app, err, reason.as_bytes());
                // Ignore errors
                let _ = resp.send(DtpMsg::DtpCloseRet { res });
            }
            _ => (),
        }
        waker.lock().unwrap().wake();
    }
}
