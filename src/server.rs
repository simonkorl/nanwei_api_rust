use anyhow::Result;
use libc::c_int;
use mio::net::UdpSocket;
use quiche::Config;
use ring::rand::*;
use std::collections::HashMap;
use std::net;
use std::sync::{Arc, Mutex};

extern crate serde;
extern crate serde_json;

use serde::{Deserialize, Serialize};

const MAX_DATAGRAM_SIZE: usize = 1350;

struct PartialResponse {
    body: Vec<u8>,

    written: usize,
}

struct Client {
    conn: quiche::Connection,

    partial_responses: HashMap<u64, PartialResponse>,
}

type ClientMap = HashMap<quiche::ConnectionId<'static>, Arc<Mutex<Client>>>;

#[derive(Serialize, Deserialize)]
struct ClientRequest {
    apiType: String,
    client_id: i32,
}

fn server_loop(
    clients: Arc<Mutex<ClientMap>>,
    poll: Arc<Mutex<mio::Poll>>,
    events: Arc<Mutex<mio::Events>>,
    socket: Arc<Mutex<UdpSocket>>,
    config: Arc<Mutex<Config>>,
) -> Result<()> {
    let mut buf = [0; 65535];
    let mut out = [0; MAX_DATAGRAM_SIZE];

    let mut poll = poll.lock().unwrap();
    let mut events = events.lock().unwrap();

    let rng = SystemRandom::new();
    let conn_id_seed = ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &rng).unwrap();

    let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(10)).unwrap());

    loop {
        // Find the shorter timeout from all the active connections.
        //
        // TODO: use event loop that properly supports timers
        let timeout = clients
            .lock()
            .unwrap()
            .values()
            .filter_map(|c| c.lock().unwrap().conn.timeout())
            .min();
        debug!("get timeout {:?}", timeout);
        poll.poll(&mut events, timeout).unwrap();

        let socket = socket.lock().unwrap();

        // Read incoming UDP packets from the socket and feed them to quiche,
        // until there are no more packets to read.
        'read: loop {
            // If the event loop reported no events, it means that the timeout
            // has expired, so handle it without attempting to read packets. We
            // will then proceed with the send loop.
            if events.is_empty() {
                debug!("timed out");

                clients
                    .lock()
                    .unwrap()
                    .values_mut()
                    .for_each(|c| c.lock().unwrap().conn.on_timeout());

                break 'read;
            }

            let (len, from) = match socket.recv_from(&mut buf) {
                Ok(v) => v,

                Err(e) => {
                    // There are no more UDP packets to read, so end the read
                    // loop.
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        debug!("recv() would block");
                        break 'read;
                    }

                    panic!("recv() failed: {:?}", e);
                }
            };

            debug!("got {} bytes", len);

            let pkt_buf = &mut buf[..len];

            // Parse the QUIC packet's header.
            let hdr = match quiche::Header::from_slice(pkt_buf, quiche::MAX_CONN_ID_LEN) {
                Ok(v) => v,

                Err(e) => {
                    error!("Parsing packet header failed: {:?}", e);
                    continue 'read;
                }
            };

            trace!("got packet {:?}", hdr);

            let conn_id = ring::hmac::sign(&conn_id_seed, &hdr.dcid);
            let conn_id = &conn_id.as_ref()[..quiche::MAX_CONN_ID_LEN];
            let conn_id = conn_id.to_vec().into();

            // Lookup a connection based on the packet's connection ID. If there
            // is no connection matching, create a new one.
            let mut clients_lock = clients.lock().unwrap();
            let client = if !clients_lock.contains_key(&hdr.dcid)
                && !clients_lock.contains_key(&conn_id)
            {
                if hdr.ty != quiche::Type::Initial {
                    error!("Packet is not Initial");
                    continue 'read;
                }

                if !quiche::version_is_supported(hdr.version) {
                    warn!("Doing version negotiation");

                    let len = quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut out).unwrap();

                    let out = &out[..len];

                    if let Err(e) = socket.send_to(out, from) {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            debug!("send() would block");
                            break;
                        }

                        panic!("send() failed: {:?}", e);
                    }
                    continue 'read;
                }

                let mut scid = [0; quiche::MAX_CONN_ID_LEN];
                scid.copy_from_slice(&conn_id);

                let scid = quiche::ConnectionId::from_ref(&scid);

                // Token is always present in Initial packets.
                let token = hdr.token.as_ref().unwrap();

                // Do stateless retry if the client didn't send a token.
                if token.is_empty() {
                    warn!("Doing stateless retry");

                    let new_token = mint_token(&hdr, &from);

                    let len = quiche::retry(
                        &hdr.scid,
                        &hdr.dcid,
                        &scid,
                        &new_token,
                        hdr.version,
                        &mut out,
                    )
                    .unwrap();

                    let out = &out[..len];

                    if let Err(e) = socket.send_to(out, from) {
                        if e.kind() == std::io::ErrorKind::WouldBlock {
                            debug!("send() would block");
                            break;
                        }

                        panic!("send() failed: {:?}", e);
                    }
                    continue 'read;
                }

                let odcid = validate_token(&from, token);

                // The token was not valid, meaning the retry failed, so
                // drop the packet.
                if odcid.is_none() {
                    error!("Invalid address validation token");
                    continue 'read;
                }

                if scid.len() != hdr.dcid.len() {
                    error!("Invalid destination connection ID");
                    continue 'read;
                }

                // Reuse the source connection ID we sent in the Retry packet,
                // instead of changing it again.
                let scid = hdr.dcid.clone();

                debug!("New connection: dcid={:?} scid={:?}", hdr.dcid, scid);

                let mut config = config.lock().unwrap();
                let conn = quiche::accept(
                    &scid,
                    odcid.as_ref(),
                    // local_addr,
                    from,
                    &mut config,
                )
                .unwrap();

                let client = Client {
                    conn,
                    partial_responses: HashMap::new(),
                };

                clients_lock.insert(scid.clone(), Arc::new(Mutex::new(client)));

                clients_lock.get_mut(&scid).unwrap()
            } else {
                match clients_lock.get_mut(&hdr.dcid) {
                    Some(v) => v,

                    None => clients_lock.get_mut(&conn_id).unwrap(),
                }
            };

            let recv_info = quiche::RecvInfo {
                // to: socket.local_addr().unwrap(),
                from,
            };

            // Process potentially coalesced packets.
            let read = match client.lock().unwrap().conn.recv(pkt_buf, recv_info) {
                Ok(v) => v,

                Err(e) => {
                    error!(
                        "{} recv failed: {:?}",
                        client.lock().unwrap().conn.trace_id(),
                        e
                    );
                    continue 'read;
                }
            };

            debug!(
                "{} processed {} bytes",
                client.lock().unwrap().conn.trace_id(),
                read
            );

            if client.lock().unwrap().conn.is_in_early_data()
                || client.lock().unwrap().conn.is_established()
            {
                // Handle writable streams.
                {
                    let mut client_lock = client.lock().unwrap();
                    for stream_id in client_lock.conn.writable() {
                        debug!("in for");
                        handle_writable(&mut client_lock, stream_id);
                    }
                }
                {
                    let mut client_lock = client.lock().unwrap();
                    // Process all readable streams.
                    for s in client_lock.conn.readable() {
                        while let Ok((read, fin)) = client_lock.conn.stream_recv(s, &mut buf) {
                            debug!("{} received {} bytes", client_lock.conn.trace_id(), read);

                            let stream_buf = &buf[..read];

                            debug!(
                                "{} stream {} has {} bytes (fin? {})",
                                client_lock.conn.trace_id(),
                                s,
                                stream_buf.len(),
                                fin
                            );

                            let a = Arc::clone(client);
                            let async_buf = buf[..read].to_owned();
                            let waker_clone = waker.clone();
                            info!("before spawn {}", client_lock.conn.trace_id());

                            std::thread::spawn(move || {
                                info!("inside spawn");
                                info!("spawn for {}", a.clone().lock().unwrap().conn.trace_id());
                                handle_stream(
                                    a,
                                    s,
                                    async_buf.as_slice(),
                                    "examples/root",
                                    waker_clone,
                                );
                            });
                            // let h = tokio::spawn(async move {
                            //     info!("inside spawn");
                            //     info!("spawn for {}", a.clone().lock().unwrap().conn.trace_id());
                            //     handle_stream(a, s, async_buf.as_slice(), "examples/root", waker_clone);
                            // });
                            // info!("get handle {:?}", h);
                        }
                    }
                }
            }
        }

        // Generate outgoing QUIC packets for all active connections and send
        // them on the UDP socket, until quiche reports that there are no more
        // packets to be sent.
        for client in clients.lock().unwrap().values_mut() {
            loop {
                let mut client = client.lock().unwrap();
                let (write, send_info) = match client.conn.send(&mut out) {
                    Ok(v) => v,

                    Err(quiche::Error::Done) => {
                        debug!("{} done writing", client.conn.trace_id());
                        break;
                    }

                    Err(e) => {
                        error!("{} send failed: {:?}", client.conn.trace_id(), e);

                        client.conn.close(false, 0x1, b"fail").ok();
                        break;
                    }
                };

                if let Err(e) = socket.send_to(&out[..write], send_info.to) {
                    if e.kind() == std::io::ErrorKind::WouldBlock {
                        debug!("send() would block");
                        break;
                    }

                    panic!("send() failed: {:?}", e);
                }

                debug!("{} written {} bytes", client.conn.trace_id(), write);
            }
        }

        // Garbage collect closed connections.
        clients.lock().unwrap().retain(|_, ref mut c| {
            debug!("Collecting garbage");
            let c = c.lock().unwrap();
            if c.conn.is_closed() {
                info!(
                    "{} connection collected {:?}",
                    c.conn.trace_id(),
                    c.conn.stats()
                );
            }

            !c.conn.is_closed()
        });
    }
}

/// Generate a stateless retry token.
///
/// The token includes the static string `"quiche"` followed by the IP address
/// of the client and by the original destination connection ID generated by the
/// client.
///
/// Note that this function is only an example and doesn't do any cryptographic
/// authenticate of the token. *It should not be used in production system*.
fn mint_token(hdr: &quiche::Header, src: &net::SocketAddr) -> Vec<u8> {
    let mut token = Vec::new();

    token.extend_from_slice(b"quiche");

    let addr = match src.ip() {
        std::net::IpAddr::V4(a) => a.octets().to_vec(),
        std::net::IpAddr::V6(a) => a.octets().to_vec(),
    };

    token.extend_from_slice(&addr);
    token.extend_from_slice(&hdr.dcid);

    token
}

/// Validates a stateless retry token.
///
/// This checks that the ticket includes the `"quiche"` static string, and that
/// the client IP address matches the address stored in the ticket.
///
/// Note that this function is only an example and doesn't do any cryptographic
/// authenticate of the token. *It should not be used in production system*.
fn validate_token<'a>(src: &net::SocketAddr, token: &'a [u8]) -> Option<quiche::ConnectionId<'a>> {
    if token.len() < 6 {
        return None;
    }

    if &token[..6] != b"quiche" {
        return None;
    }

    let token = &token[6..];

    let addr = match src.ip() {
        std::net::IpAddr::V4(a) => a.octets().to_vec(),
        std::net::IpAddr::V6(a) => a.octets().to_vec(),
    };

    if token.len() < addr.len() || &token[..addr.len()] != addr.as_slice() {
        return None;
    }

    Some(quiche::ConnectionId::from_ref(&token[addr.len()..]))
}

/// Handles incoming HTTP/0.9 requests.
#[tokio::main]
async fn handle_stream(
    client: Arc<Mutex<Client>>,
    stream_id: u64,
    buf: &[u8],
    _root: &str,
    waker: Arc<mio::Waker>,
) {
    // let conn = &mut client.conn;

    // if buf.len() > 4 && &buf[..4] == b"GET " {
    //     let uri = &buf[4..buf.len()];
    //     let uri = String::from_utf8(uri.to_vec()).unwrap();
    //     let uri = String::from(uri.lines().next().unwrap());
    //     let uri = std::path::Path::new(&uri);
    //     let mut path = std::path::PathBuf::from(root);

    //     for c in uri.components() {
    //         if let std::path::Component::Normal(v) = c {
    //             path.push(v)
    //         }
    //     }

    //     info!(
    //         "{} got GET request for {:?} on stream {}",
    //         conn.trace_id(),
    //         path,
    //         stream_id
    //     );

    //     let body = std::fs::read(path.as_path())
    //         .unwrap_or_else(|_| b"Not Found!\r\n".to_vec());

    //     info!(
    //         "{} sending response of size {} on stream {}",
    //         conn.trace_id(),
    //         body.len(),
    //         stream_id
    //     );

    //     let written = match conn.stream_send(stream_id, &body, true) {
    //         Ok(v) => v,

    //         Err(quiche::Error::Done) => 0,

    //         Err(e) => {
    //             error!("{} stream send failed {:?}", conn.trace_id(), e);
    //             return;
    //         },
    //     };

    //     if written < body.len() {
    //         let response = PartialResponse { body, written };
    //         client.partial_responses.insert(stream_id, response);
    //     }
    // }

    let data = &buf[0..buf.len()];
    let data = String::from_utf8(data.to_vec()).unwrap();

    let req: ClientRequest = serde_json::from_str(data.as_str()).unwrap();

    info!(
        "{} got request {} from id {:?} on stream {}",
        client.lock().unwrap().conn.trace_id(),
        data,
        req.client_id,
        stream_id
    );

    // 模拟进行处理
    std::thread::sleep(std::time::Duration::from_secs_f64(1.0)); // 同步的线程阻塞
                                                                 // tokio::time::sleep(std::time::Duration::from_secs_f64(0.5)).await; // 异步阻塞
    let res = format!("收到：{}\n", data);

    info!(
        "{} sending response of size {} on stream {}",
        client.lock().unwrap().conn.trace_id(),
        res.len(),
        stream_id
    );

    let mut client_lock = client.lock();
    let client: &mut Client = client_lock.as_mut().unwrap();
    let conn = &mut client.conn;
    let written = match conn.stream_send(stream_id, res.as_bytes(), true) {
        Ok(v) => v,

        Err(quiche::Error::Done) => 0,

        Err(e) => {
            error!("{} stream send failed {:?}", conn.trace_id(), e);
            return;
        }
    };

    if written < res.len() {
        let response = PartialResponse {
            body: res.as_bytes().to_owned(),
            written,
        };
        client.partial_responses.insert(stream_id, response);
    }

    // TODO: 创建事件，声明可以发送数据
    debug!("try to send in handle stream: {}", client.conn.trace_id());
    waker.wake().expect("failed to wake");
}

/// Handles newly writable streams.
fn handle_writable(client: &mut Client, stream_id: u64) {
    // let mut client_lock = client.lock();
    // let client: &mut Client = client_lock.as_mut().unwrap();

    let conn = &mut client.conn;

    debug!("{} stream {} is writable", conn.trace_id(), stream_id);

    if !client.partial_responses.contains_key(&stream_id) {
        return;
    }

    let resp = client.partial_responses.get_mut(&stream_id).unwrap();
    let body = &resp.body[resp.written..];

    let written = match conn.stream_send(stream_id, body, true) {
        Ok(v) => v,

        Err(quiche::Error::Done) => 0,

        Err(e) => {
            client.partial_responses.remove(&stream_id);

            error!("{} stream send failed {:?}", conn.trace_id(), e);
            return;
        }
    };

    resp.written += written;

    if resp.written == resp.body.len() {
        client.partial_responses.remove(&stream_id);
    }
}

#[repr(C)]
#[derive(Default)]
pub struct DtpServer {
    clients: Option<Arc<Mutex<ClientMap>>>,
    pub socket: Option<Arc<Mutex<UdpSocket>>>,
    poll: Option<Arc<Mutex<mio::Poll>>>, // 注册 socket 之后只在 client 循环中被引用
    events: Option<Arc<Mutex<mio::Events>>>, // 只会在 server_loop 中被引用
    pub config: Option<Arc<Mutex<Config>>>,
    sockid: Option<c_int>,
}

impl DtpServer {
    pub fn run(&self) {
        let clients = self.clients.clone().unwrap().clone();
        let p = self.poll.clone().unwrap().clone();
        let e = self.events.clone().unwrap().clone();
        let s = self.socket.clone().unwrap().clone();
        let c = self.config.clone().unwrap().clone();
        let h = std::thread::spawn(move || {
            server_loop(clients, p, e, s, c).unwrap();
        });
        h.join().unwrap();
    }

    /// 根据目的地址创建一个 DtpServer
    /// 其可以通过调用 run 来运行
    pub fn listen(ip: String, port: i32, config: Arc<Mutex<Config>>) -> Result<DtpServer> {
        // Setup the event loop.
        let poll = mio::Poll::new()?;
        let events = mio::Events::with_capacity(2048);

        let local_addr = format!("{}:{}", ip, port).parse()?;

        // Create the UDP listening socket, and register it with the event loop.
        let mut socket = mio::net::UdpSocket::bind(local_addr)?;

        poll.registry()
            .register(&mut socket, mio::Token(0), mio::Interest::READABLE)?;

        let clients = Arc::new(Mutex::new(ClientMap::new()));

        Ok(DtpServer {
            poll: Some(Arc::new(Mutex::new(poll))),
            events: Some(Arc::new(Mutex::new(events))),
            clients: Some(clients),
            socket: Some(Arc::new(Mutex::new(socket))),
            config: Some(config),
            sockid: None,
        })
    }

    /// 根据目的地址创建一个 DtpServer
    ///
    /// 这个 server 没有 config，无法运行
    pub fn bind(ip: String, port: i32, sockid: c_int) -> Result<DtpServer> {
        // Setup the event loop.
        let poll = mio::Poll::new()?;
        let events = mio::Events::with_capacity(2048);

        let local_addr = format!("{}:{}", ip, port).parse()?;

        // Create the UDP listening socket, and register it with the event loop.
        let mut socket = mio::net::UdpSocket::bind(local_addr)?;

        poll.registry()
            .register(&mut socket, mio::Token(0), mio::Interest::READABLE)?;

        let clients = Arc::new(Mutex::new(ClientMap::new()));

        Ok(DtpServer {
            poll: Some(Arc::new(Mutex::new(poll))),
            events: Some(Arc::new(Mutex::new(events))),
            clients: Some(clients),
            socket: Some(Arc::new(Mutex::new(socket))),
            config: None,
            sockid: Some(sockid),
        })
    }
}
