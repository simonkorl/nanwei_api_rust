# 测试程序说明

## udp_high_test

该程序用于测试和解释在高并发的场景下为什么 dtp_api 的使用可能出现问题

这个程序展示出，如果同时的并发数太高，那么 UdpSocket 和 Poll 之间的交互可能会产生令人疑惑的现象。

使用 `cargo run --example udp_high_test 1000 > test.log`，并且在 test.log 中搜索 `recv 1200 of Reply:` 会发现数量不为 1000，并且每一次测试的时候会发生随机变化。将 1000 改为 100 等更小的数字会明显降低错误发生的概率。

这可能说明如果一次性创建大量的 client ，并同时向 server 发送信息会导致 UdpSocket 的行为异常，很可能是因为大量的包互相冲撞导致的问题。

如果要解决 Udp 行为的异常，那么可以编写一个 echo 的逻辑：如果对方没有在规定的时间内收到数据则重发。直到目的行为达成为止。在 DTP 中，这个问题应该可以更好解决一点，就是需要在部分 API 中添加一个时钟，并且编写一个线程池和等待队列，不要同时处理太多的客户端，否则会产生问题。

2023.12.12 更新：根据排查，这个问题是一个 wouldblock 问题。根据 https://github.com/tokio-rs/mio/issues/1076 的回答，我们只需要在 trigger 事件之后持续调用 udp recv 直到出现 would block 错误为止。经过这样的修改后就可以解决并发 1000 个客户端的问题了。哪怕同时创建 2000 个客户端线程也不会出现问题。

## test_client , test_server

使用 DTP API 进行测试的 Rust 程序，可以直接运行进行测试。

```sh
cargo run --example test_server
cargo run --example test_client 250 > client.log
```