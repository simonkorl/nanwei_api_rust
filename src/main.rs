use nanwei_api_rust::client::DtpClient;
use nanwei_api_rust::server::DtpServer;
use nanwei_api_rust::ffi::dtp_config_init;
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    env_logger::init();
    // 模拟启动 server
    let server_handle = std::thread::spawn(||{
        let config_ptr = dtp_config_init();
        let mut config = unsafe { Box::from_raw(config_ptr) };
        config.set_max_idle_timeout(10000);

        let config_arc = Arc::new(Mutex::new(*config));
        let server = DtpServer::listen("127.0.0.1".to_owned(), 4433, config_arc).unwrap();

        server.run();
    });

    // 模拟客户端程序
    let mut handles = vec![];
    for i in 0..500 {
        let h = tokio::spawn(async move {
            let config_ptr = dtp_config_init();
            let mut config = unsafe { Box::from_raw(config_ptr) };
            config.set_max_idle_timeout(10000);

            let mut client = DtpClient::connect("127.0.0.1".to_owned(), 4433, &mut config).unwrap();
            client.run(i);
        });
        handles.push(h);
    }

    futures::future::join_all(handles).await;
    server_handle.join().unwrap();
}
