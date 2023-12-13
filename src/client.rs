use libc::c_int;
use mio::net::UdpSocket;
use quiche::{Config, Connection};
use ring::rand::*;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Result;

const MAX_DATAGRAM_SIZE: usize = 1350;

const HTTP_REQ_STREAM_ID: u64 = 4;

fn client_req_str(id: i32) -> String {
    format!("{{\"apiType\": \"get\", \"client_id\": {}}}", id)
}

fn hex_dump(buf: &[u8]) -> String {
    let vec: Vec<String> = buf.iter().map(|b| format!("{b:02x}")).collect();

    vec.join("")
}

fn client_loop(
    poll: &mut mio::Poll,
    events: &mut mio::Events,
    conn: Arc<Mutex<Connection>>,
    socket: Arc<Mutex<UdpSocket>>,
    id: i32,
) -> Result<()> {
    let mut buf = [0; 65535];
    let mut out = [0; MAX_DATAGRAM_SIZE];

    loop {
        let timeout = conn.lock().unwrap().timeout();
        debug!("{} get timeout {:?}", id, timeout);
        poll.poll(events, timeout)?;

        // Read incoming UDP packets from the socket and feed them to quiche,
        // until there are no more packets to read.
        'read: loop {
            let mut conn = conn.lock().unwrap();
            let socket = socket.lock().unwrap();
            // If the event loop reported no events, it means that the timeout
            // has expired, so handle it without attempting to read packets. We
            // will then proceed with the send loop.
            if events.is_empty() {
                debug!("timed out");

                conn.on_timeout();
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

            let recv_info = quiche::RecvInfo {
                // to: socket.local_addr().unwrap(),
                from,
            };

            // Process potentially coalesced packets.
            let read = match conn.recv(&mut buf[..len], recv_info) {
                Ok(v) => v,

                Err(e) => {
                    error!("recv failed: {:?}", e);
                    continue 'read;
                }
            };

            debug!("processed {} bytes", read);
        }

        debug!("done reading");

        // TODO: 是否要这个时候退出？
        if conn.lock().unwrap().is_closed() {
            info!("connection closed, {:?}", conn.lock().unwrap().stats());
            break;
        }

        // let is_in_early_data = conn.lock().unwrap().is_in_early_data();
        // let is_established = conn.lock().unwrap().is_established();
        // if is_in_early_data || is_established {
        // }

        // Process all readable streams.
        // 这部分逻辑靠外部 api 实现，这里不再需要处理
        {}

        // Generate outgoing QUIC packets and send them on the UDP socket, until
        // quiche reports that there are no more packets to be sent.
        loop {
            let mut conn = conn.lock().unwrap();
            let (write, send_info) = match conn.send(&mut out) {
                Ok(v) => v,

                Err(quiche::Error::Done) => {
                    debug!("done writing");
                    break;
                }

                Err(e) => {
                    error!("send failed: {:?}", e);

                    conn.close(false, 0x1, b"fail").ok();
                    break;
                }
            };

            let socket = socket.lock().unwrap();

            if let Err(e) = socket.send_to(&out[..write], send_info.to) {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    debug!("send() would block");
                    break;
                }

                panic!("send() failed: {:?}", e);
            }

            debug!("written {}", write);
        }

        if conn.lock().unwrap().is_closed() {
            info!("connection closed, {:?}", conn.lock().unwrap().stats());
            break;
        }
    }
    Ok(())
}
#[repr(C)]
pub struct DtpClient {
    conn: Arc<Mutex<Connection>>, // 在客户端调用函数 connect 之后获得
    socket: Arc<Mutex<UdpSocket>>,
    waker: Arc<Mutex<mio::Waker>>,
    poll: mio::Poll,     // 注册 socket 之后只在 client 循环中被引用
    events: mio::Events, // 只会在 client_loop 中被引用
    peer_addr: SocketAddr,
    sockid: c_int,
}

impl DtpClient {
    /// 在 connect 之后运行，持续运行
    // pub fn run(&self, id: i32) {
    //     let p = self.poll.clone();
    //     let e = self.events.clone();
    //     let c = self.conn.clone();
    //     let s = self.socket.clone();
    //     let peer = self.peer_addr.clone();
    //     let h = std::thread::spawn(move || {
    //         client_loop(p, e, c, s, peer, id).unwrap();
    //     });
    //     h.join().unwrap();
    // }

    pub fn run(&mut self, id: i32) -> Result<()> {
        client_loop(&mut self.poll, &mut self.events, self.conn.clone(), self.socket.clone(), id)
    }

    pub fn get_socket(&self) -> Arc<Mutex<UdpSocket>> {
        self.socket.clone()
    }

    pub fn get_waker(&self) -> Arc<Mutex<mio::Waker>> {
        self.waker.clone()
    }

    pub fn get_conn(&self) -> Arc<Mutex<quiche::Connection>> {
        self.conn.clone()
    }

    /// 在 bind 之后调用，进行连接
    /// 可以使用这个函数直接返回一个 DtpClient
    pub fn connect(ip: String, port: i32, config: &mut Config, sockid: c_int) -> Result<DtpClient> {
        // Setup the event loop.
        let poll = mio::Poll::new().unwrap();
        let events = mio::Events::with_capacity(2048);

        let peer_addr = format!("{}:{}", ip, port).parse()?;
        // Bind to INADDR_ANY or IN6ADDR_ANY depending on the IP family of the
        // server address. This is needed on macOS and BSD variants that don't
        // support binding to IN6ADDR_ANY for both v4 and v6.
        let bind_addr = match peer_addr {
            std::net::SocketAddr::V4(_) => "0.0.0.0:0",
            std::net::SocketAddr::V6(_) => "[::]:0",
        };

        // Create the UDP socket backing the QUIC connection, and register it with
        // the event loop.
        let mut socket = mio::net::UdpSocket::bind(bind_addr.parse()?)?;

        poll.registry()
            .register(&mut socket, mio::Token(0), mio::Interest::READABLE)
            .unwrap();

        let waker = mio::Waker::new(poll.registry(), mio::Token(42)).unwrap();

        let socket_arc = Arc::new(Mutex::new(socket));

        let socket_arc_clone = socket_arc.clone();

        let mut out = [0; MAX_DATAGRAM_SIZE];

        let socket = socket_arc.lock().unwrap();
        // Generate a random source connection ID for the connection.
        let mut scid = [0; quiche::MAX_CONN_ID_LEN];
        SystemRandom::new().fill(&mut scid[..]).unwrap();

        let scid = quiche::ConnectionId::from_ref(&scid);
        // Create a QUIC connection and initiate handshake.
        let mut conn = quiche::connect(
            Some("server"),
            &scid,
            // local_addr,
            peer_addr,
            config,
        )?;

        info!(
            "connecting to {:} from {:} with scid {}",
            peer_addr,
            socket.local_addr().unwrap(),
            hex_dump(&scid)
        );

        let (write, send_info) = conn.send(&mut out).expect("initial send failed");

        while let Err(e) = socket.send_to(&out[..write], send_info.to) {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                debug!("send() would block");
                continue;
            }

            panic!("send() failed: {:?}", e);
        }

        debug!("written {}", write);

        Ok(DtpClient {
            conn: Arc::new(Mutex::new(conn)),
            socket: socket_arc_clone,
            peer_addr: peer_addr,
            poll: poll,
            events: events,
            sockid: sockid,
            waker: Arc::new(Mutex::new(waker)),
        })
    }
}
