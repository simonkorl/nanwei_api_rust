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

pub struct PartialResponse {
    body: Vec<u8>,

    written: usize,
}

#[derive(Default)]
pub struct SharedStreams {
    // pub readables: HashMap<u64, (Vec<u8>, bool)>,
    pub client_status:
        HashMap<quiche::ConnectionId<'static>, (Arc<Mutex<quiche::Connection>>, ClientStatus)>,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum ClientStatus {
    NotConnected,
    Connected,
    Accepted,
    Close,
    Wait,
}

pub struct Client {
    pub conn: Arc<Mutex<quiche::Connection>>,

    pub partial_responses: Arc<Mutex<HashMap<u64, PartialResponse>>>,
    // pub status: Arc<Mutex<ClientStatus>>,

    // pub scid: quiche::ConnectionId<'static>,
}

type ClientMap = HashMap<quiche::ConnectionId<'static>, Client>;

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize)]
struct ClientRequest {
    apiType: String,
    client_id: i32,
}

struct ServerArgs<'a> {
    clients: &'a mut ClientMap,
    poll: &'a mut mio::Poll,
    events: &'a mut mio::Events,
    socket: Arc<Mutex<UdpSocket>>,
    config: Arc<Mutex<Config>>,
    shared_streams: Arc<Mutex<SharedStreams>>,
}

fn server_loop(args: ServerArgs) -> Result<()> {
    let mut buf = [0; 65535];
    let mut out = [0; MAX_DATAGRAM_SIZE];

    let clients = args.clients;
    let poll = args.poll;
    let events = args.events;
    let socket = args.socket;
    let config = args.config;
    let shared_streams = args.shared_streams;

    let rng = SystemRandom::new();
    let conn_id_seed = ring::hmac::Key::generate(ring::hmac::HMAC_SHA256, &rng).unwrap();

    let _msg_count = 0;

    // let waker = Arc::new(mio::Waker::new(poll.registry(), mio::Token(10)).unwrap());

    loop {
        // Find the shorter timeout from all the active connections.
        //
        // TODO: use event loop that properly supports timers
        debug!("begin of loop");
        let timeout = clients
            .values()
            .filter_map(|c| c.conn.lock().unwrap().timeout())
            .min();
        debug!("get timeout {:?}", timeout);
        poll.poll(events, timeout).unwrap();
        debug!("after poll");
        let socket = socket.lock().unwrap();
        debug!("before reading");
        // Read incoming UDP packets from the socket and feed them to quiche,
        // until there are no more packets to read.
        'read: loop {
            // If the event loop reported no events, it means that the timeout
            // has expired, so handle it without attempting to read packets. We
            // will then proceed with the send loop.
            if events.is_empty() {
                debug!("timed out");

                clients
                    .values_mut()
                    .for_each(|c| c.conn.lock().unwrap().on_timeout());

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
            let client = if !clients.contains_key(&hdr.dcid) && !clients.contains_key(&conn_id) {
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

                let conn_arc = Arc::new(Mutex::new(conn));

                let client = Client {
                    conn: conn_arc.clone(),
                    partial_responses: Arc::new(Mutex::new(HashMap::new())),
                    // status: Arc::new(Mutex::new(ClientStatus::Connected)),
                    // scid: scid.clone(),
                };

                clients.insert(scid.clone(), client);

                debug!("before status lock");
                let mut shared_streams_lock = shared_streams.lock().unwrap();
                shared_streams_lock
                    .client_status
                    .insert(scid.clone(), (conn_arc.clone(), ClientStatus::Connected));
                debug!("after status lock");
                clients.get_mut(&scid).unwrap()
            } else {
                match clients.get_mut(&hdr.dcid) {
                    Some(v) => v,

                    None => clients.get_mut(&conn_id).unwrap(),
                }
            };

            let recv_info = quiche::RecvInfo {
                // to: socket.local_addr().unwrap(),
                from,
            };

            // Process potentially coalesced packets.
            let read = match client.conn.lock().unwrap().recv(pkt_buf, recv_info) {
                Ok(v) => v,

                Err(e) => {
                    error!(
                        "{} recv failed: {:?}",
                        client.conn.lock().unwrap().trace_id(),
                        e
                    );
                    continue 'read;
                }
            };

            debug!(
                "{} processed {} bytes",
                client.conn.lock().unwrap().trace_id(),
                read
            );

            let is_in_early_data = client.conn.lock().unwrap().is_in_early_data();
            let is_established = client.conn.lock().unwrap().is_established();
            if is_in_early_data || is_established {
                {
                    debug!("handle writable");
                    // Handle writable streams.
                    // 此处会将 partial_responses 中的全部数据，根据流的 writable
                    // 属性进行输出
                    // handle_writable 函数可以保证只要在 partial_responses 中的数据
                    // 都可以完整地发送到对方
                    let conn = client.conn.clone();
                    let c = client.conn.clone();
                    let partial_responses = client.partial_responses.clone();
                    let writable = { c.lock().unwrap().writable() };
                    for stream_id in writable {
                        debug!("in for");
                        handle_writable(conn.clone(), partial_responses.clone(), stream_id);
                    }
                }
                {
                    // Process all readable streams.
                    // TODO: 考虑把数据从 stream 中读出到一个中间 cache 当中，这样就可以百分之百同步
                    // TODO: 处理 loop，防止出现任何问题
                    // let readable = client.conn.lock().unwrap().readable();
                    // let mut shared_streams_lock = shared_streams.lock().unwrap();
                    // for s in readable {
                    //     loop {
                    //         let recv = client.conn.lock().unwrap().stream_recv(s, &mut buf);
                    //         match recv {
                    //             Ok((read, fin)) => {
                    //                 info!(
                    //                     "{} received {} bytes",
                    //                     client.conn.lock().unwrap().trace_id(),
                    //                     read
                    //                 );

                    //                 let stream_buf = &buf[..read];

                    //                 info!(
                    //                     "{} stream {} has {} bytes (fin? {})",
                    //                     client.conn.lock().unwrap().trace_id(),
                    //                     s,
                    //                     stream_buf.len(),
                    //                     fin
                    //                 );

                    //                 if let Some((r, f)) = shared_streams_lock.readables.get_mut(&s) {
                    //                     r.append(&mut stream_buf.to_vec());
                    //                     *f = fin;
                    //                 } else {
                    //                     shared_streams_lock.readables.insert(s, (stream_buf.to_vec(), fin));
                    //                 }
                    //             }
                    //             Err(_) => break,
                    //         }
                    //     }
                    //  debug!("in readable {}", s);
                    // }
                }
                debug!("after readable");
            }
        }

        // Generate outgoing QUIC packets for all active connections and send
        // them on the UDP socket, until quiche reports that there are no more
        // packets to be sent.
        debug!("begin writing");
        for client in clients.values_mut() {
            debug!(
                "write loop: client {}",
                client.conn.clone().lock().unwrap().trace_id()
            );
            loop {
                let mut conn_lock = client.conn.lock().unwrap();

                let (write, send_info) = match conn_lock.send(&mut out) {
                    Ok(v) => v,

                    Err(quiche::Error::Done) => {
                        debug!("{} done writing", conn_lock.trace_id());
                        break;
                    }

                    Err(e) => {
                        error!("{} send failed: {:?}", conn_lock.trace_id(), e);

                        conn_lock.close(false, 0x1, b"fail").ok();
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

                debug!("{} written {} bytes", conn_lock.trace_id(), write);
            }
        }

        // Garbage collect closed connections.
        debug!("before garbage");
        {
            let client_status = &shared_streams.lock().unwrap().client_status;
            clients.retain(|_, ref mut c| {
                let conn_lock = c.conn.lock().unwrap();
                let scid = conn_lock.source_id();
                if let Some((_, conn_status)) = client_status.get(&scid) {
                    if conn_lock.is_closed() {
                        if conn_status == &ClientStatus::Close {
                            debug!(
                                "{} connection collected {:?}",
                                conn_lock.trace_id(),
                                conn_lock.stats()
                            );
                        } else {
                            debug!(
                                "{} connection cannot be collected, need client to close",
                                conn_lock.trace_id(),
                            );
                        }
                    }

                    !conn_lock.is_closed() || conn_status != &ClientStatus::Close
                } else {
                    unreachable!();
                }
            });
        }
        debug!("after garbage");
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
///
/// 这个异步处理可以用来模拟服务器对于数据的处理
/// 这个函数不再会在 server_loop 中被直接调用
#[tokio::main]
async fn handle_stream(
    conn: Arc<Mutex<quiche::Connection>>,
    partial_responses: Arc<Mutex<HashMap<u64, PartialResponse>>>,
    stream_id: u64,
    buf: &[u8],
    _root: &str,
    waker: Arc<Mutex<mio::Waker>>,
) {
    let data = &buf[0..buf.len()];
    let data = String::from_utf8(data.to_vec()).unwrap();

    let req: ClientRequest = serde_json::from_str(data.as_str()).unwrap();

    info!(
        "{} got request {} from id {:?} on stream {}",
        conn.lock().unwrap().trace_id(),
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
        conn.lock().unwrap().trace_id(),
        res.len(),
        stream_id
    );

    let written = match conn
        .lock()
        .unwrap()
        .stream_send(stream_id, res.as_bytes(), true)
    {
        Ok(v) => v,

        Err(quiche::Error::Done) => 0,

        Err(e) => {
            error!(
                "{} stream send failed {:?}",
                conn.lock().unwrap().trace_id(),
                e
            );
            return;
        }
    };

    if written < res.len() {
        let response = PartialResponse {
            body: res.as_bytes().to_owned(),
            written,
        };
        partial_responses
            .lock()
            .unwrap()
            .insert(stream_id, response);
    }

    // TODO: 创建事件，声明可以发送数据
    debug!(
        "try to send in handle stream: {}",
        conn.lock().unwrap().trace_id()
    );
    waker.lock().unwrap().wake().expect("failed to wake");
}

/// Handles newly writable streams.
fn handle_writable(
    conn: Arc<Mutex<quiche::Connection>>,
    partial_responses: Arc<Mutex<HashMap<u64, PartialResponse>>>,
    stream_id: u64,
) {
    let mut conn = conn.lock().unwrap();
    let mut partial_responses = partial_responses.lock().unwrap();

    debug!("{} stream {} is writable", conn.trace_id(), stream_id);

    if !partial_responses.contains_key(&stream_id) {
        return;
    }

    let resp = partial_responses.get_mut(&stream_id).unwrap();
    let body = &resp.body[resp.written..];

    let written = match conn.stream_send(stream_id, body, true) {
        Ok(v) => v,

        Err(quiche::Error::Done) => 0,

        Err(e) => {
            partial_responses.remove(&stream_id);

            error!("{} stream send failed {:?}", conn.trace_id(), e);
            return;
        }
    };

    resp.written += written;

    if resp.written == resp.body.len() {
        partial_responses.remove(&stream_id);
    }
}

#[repr(C)]
pub struct DtpServer {
    clients: ClientMap,
    poll: mio::Poll,     // 注册 socket 之后只在 client 循环中被引用
    events: mio::Events, // 只会在 server_loop 中被引用
    socket: Arc<Mutex<UdpSocket>>,
    waker: Arc<Mutex<mio::Waker>>,
    shared_streams: Arc<Mutex<SharedStreams>>,
    config: Option<Arc<Mutex<Config>>>,
    sockid: Option<c_int>,
}

impl DtpServer {
    pub fn run(&mut self) -> Result<()> {
        let args = self.get_run_args();
        server_loop(args)
    }

    pub fn get_waker(&self) -> Arc<Mutex<mio::Waker>> {
        self.waker.clone()
    }

    pub fn get_socket(&self) -> Arc<Mutex<UdpSocket>> {
        self.socket.clone()
    }

    pub fn get_shared_streams(&self) -> Arc<Mutex<SharedStreams>> {
        self.shared_streams.clone()
    }

    pub fn set_config(&mut self, config_arc: Arc<Mutex<Config>>) -> bool {
        self.config = Some(config_arc.clone());
        true
    }

    fn get_run_args(&mut self) -> ServerArgs {
        ServerArgs {
            clients: &mut self.clients,
            poll: &mut self.poll,
            events: &mut self.events,
            socket: self.socket.clone(),
            config: self.config.clone().unwrap().clone(),
            shared_streams: self.shared_streams.clone(),
        }
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

        let waker = mio::Waker::new(poll.registry(), mio::Token(42)).unwrap();

        let clients = ClientMap::new();

        Ok(DtpServer {
            poll,
            events,
            clients,
            socket: Arc::new(Mutex::new(socket)),
            shared_streams: Arc::new(Mutex::new(SharedStreams::default())),
            config: Some(config),
            sockid: None,
            waker: Arc::new(Mutex::new(waker)),
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

        let waker = mio::Waker::new(poll.registry(), mio::Token(42)).unwrap();

        let clients = ClientMap::new();

        Ok(DtpServer {
            poll,
            events,
            clients,
            socket: Arc::new(Mutex::new(socket)),
            shared_streams: Arc::new(Mutex::new(SharedStreams::default())),
            config: None,
            sockid: Some(sockid),
            waker: Arc::new(Mutex::new(waker)),
        })
    }
}
