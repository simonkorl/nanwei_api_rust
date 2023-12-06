use mio::net::UdpSocket;
use quiche::{Config, Connection};
use std::sync::{Arc, Mutex};
use std::net::SocketAddr;
use ring::rand::*;
use anyhow::anyhow;
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
    poll: Arc<Mutex<mio::Poll>>,
    events: Arc<Mutex<mio::Events>>, 
    conn: Arc<Mutex<Connection>>,
    socket: Arc<Mutex<UdpSocket>>,
    peer_addr: SocketAddr,
    id: i32) -> Result<()> {
    let mut buf = [0; 65535];
    let mut out = [0; MAX_DATAGRAM_SIZE];

    let mut poll = poll.lock().unwrap();
    let mut events = events.lock().unwrap();

    let req_start = std::time::Instant::now();

    let mut req_sent = false;

    loop {
        let timeout = conn.lock().unwrap().timeout();
        poll.poll(&mut events, timeout)?;

        let mut conn = conn.lock().unwrap();
        // Read incoming UDP packets from the socket and feed them to quiche,
        // until there are no more packets to read.
        'read: loop {
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

        if conn.is_closed() {
            info!("connection closed, {:?}", conn.stats());
            break;
        }

        // Send an HTTP request as soon as the connection is established.
        if conn.is_established() && !req_sent {
            info!("sending HTTP request for {:?}", peer_addr);

            // let req = format!("GET {}\r\n", url.path());
            let req = client_req_str(id);
            conn.stream_send(HTTP_REQ_STREAM_ID, req.as_bytes(), true)
                .unwrap();

            req_sent = true;
        }

        // Process all readable streams.
        for s in conn.readable() {
            while let Ok((read, fin)) = conn.stream_recv(s, &mut buf) {
                debug!("received {} bytes", read);

                let stream_buf = &buf[..read];

                debug!("stream {} has {} bytes (fin? {})", s, stream_buf.len(), fin);

                print!("{}", unsafe { std::str::from_utf8_unchecked(stream_buf) });

                // The server reported that it has no more data to send, which
                // we got the full response. Close the connection.
                if s == HTTP_REQ_STREAM_ID && fin {
                    info!("response received in {:?}, closing...", req_start.elapsed());

                    conn.close(true, 0x00, b"kthxbye").unwrap();
                }
            }
        }

        // Generate outgoing QUIC packets and send them on the UDP socket, until
        // quiche reports that there are no more packets to be sent.
        loop {
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

        if conn.is_closed() {
            info!("connection closed, {:?}", conn.stats());
            break;
        }
    }
    Ok(())
}
#[repr(C)]
#[derive(Default)]
pub struct DtpClient {
    conn: Option<Arc<Mutex<Connection>>>,    // 在客户端调用函数 connect 之后获得
    socket: Option<Arc<Mutex<UdpSocket>>>,
    peer_addr: Option<SocketAddr>,
    poll: Option<Arc<Mutex<mio::Poll>>>,     // 注册 socket 之后只在 client 循环中被引用
    events: Option<Arc<Mutex<mio::Events>>>, // 只会在 client_loop 中被引用
}

impl DtpClient {
    /// 在 connect 之后运行，持续运行
    pub fn run(&mut self, id: i32) {
        let p = self.poll.clone().unwrap().clone();
        let e = self.events.clone().unwrap().clone();
        let c = self.conn.clone().unwrap().clone();
        let s = self.socket.clone().unwrap().clone();
        let peer = self.peer_addr.unwrap().clone();
        let h = std::thread::spawn(move || {
            client_loop(
                p,
                e,
                c,
                s,
                peer,
                id
            ).unwrap();
        });
        h.join().unwrap();
    }

    /// 在 bind 之后调用，进行连接
    /// 可以使用这个函数直接返回一个 DtpClient
    pub fn connect(
        ip: String, port: i32, 
        config: &mut Config
    ) -> Result<DtpClient> {
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
        let mut socket =
            mio::net::UdpSocket::bind(bind_addr.parse()?)?;

        poll.registry()
        .register(&mut socket, mio::Token(0), mio::Interest::READABLE)
        .unwrap();

        let socket_arc = Some(Arc::new(Mutex::new(socket)));

        let mut out = [0; MAX_DATAGRAM_SIZE];

        let binding = socket_arc.clone().unwrap();
        let socket = binding.lock().unwrap();
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

        Ok(
            DtpClient { 
                conn: Some(Arc::new(Mutex::new(conn))),
                socket: socket_arc, 
                peer_addr: Some(peer_addr), 
                poll: Some(Arc::new(Mutex::new(poll))), 
                events: Some(Arc::new(Mutex::new(events)))
            }
        )
    }
}
