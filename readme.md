# 测试用 Rust api

## 安装 Rust 工具链
- rustc 1.72.0 (5680fa18f 2023-08-23)

## 构件库

`cargo build --release`

在 target/release 中找到 libnanwei_api_rust.so

## 运行测试样例测试程序

`cargo run --release > client.log 2>client_err.log`

应该可以在 client.log 程序中看到不断产生的 log 信息，大概一秒钟产生 30 个，一共产生 500 个。使用 C-c 退出程序。