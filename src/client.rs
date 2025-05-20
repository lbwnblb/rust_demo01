use std::io::{self, Write};
use std::thread;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;
use tungstenite::{connect, Message};
use url::Url;
use std::time::Duration;
// 添加reqwest库以支持代理
use tungstenite::client::IntoClientRequest;
use std::env;

fn main() {
    let stderr = io::stderr();
    let mut handle = stderr.lock();
    
    writeln!(handle, "WebSocket客户端启动中...").unwrap();
    writeln!(handle, "正在连接到服务器...").unwrap();
    
    // 连接到服务器（增加代理支持）
    let url = Url::parse("ws://127.0.0.1:9001").unwrap();
    
    // 代理设置（增加代理支持，无需认证）
    let proxy_url = "http://127.0.0.1:7890";
    
    // 设置HTTP_PROXY环境变量，tungstenite将会自动使用它
    unsafe {
        env::set_var("HTTP_PROXY", proxy_url);
        env::set_var("HTTPS_PROXY", proxy_url);
    }
    
    // 使用配置的CONNECT方法代理WebSocket连接
    let mut request = url.into_client_request().unwrap();
    
    // 使用配置好的请求连接
    let (socket, response) = tungstenite::connect(request).expect("无法连接到服务器");
    
    writeln!(handle, "已连接到服务器！").unwrap();
    writeln!(handle, "服务器握手响应状态: {}", response.status()).unwrap();
    
    // 使用Arc和Mutex包装WebSocket以便在线程间安全共享
    let socket = Arc::new(Mutex::new(socket));
    
    // 创建一个线程用于读取服务器消息
    let socket_clone = socket.clone();
    let receive_thread = thread::spawn(move || {
        let stderr = io::stderr();
        let mut handle = stderr.lock();
        
        // 从WebSocket接收消息的循环
        loop {
            // 读取消息
            writeln!(handle, "等待接收服务器消息...").unwrap();
            let msg = {
                let mut socket = socket_clone.lock().unwrap();
                match socket.read_message() {
                    Ok(msg) => msg,
                    Err(e) => {
                        writeln!(handle, "接收消息错误: {:?}", e).unwrap();
                        break;
                    }
                }
            };
            
            // 处理消息
            match msg {
                Message::Text(text) => {
                    writeln!(handle, "收到消息: {}", text).unwrap();
                }
                Message::Close(_) => {
                    writeln!(handle, "服务器关闭了连接").unwrap();
                    break;
                }
                _ => {
                    writeln!(handle, "收到其他类型消息: {:?}", msg).unwrap();
                }
            }
        }
    });
    
    // 测试直接发送消息到服务器
    for i in 1..4 {
        let test_msg = format!("测试消息 {}", i);
        println!("直接发送测试消息: {}", test_msg);
        
        let mut socket_lock = socket.lock().unwrap();
        match socket_lock.write_message(Message::Text(test_msg)) {
            Ok(_) => println!("测试消息发送成功!"),
            Err(e) => println!("测试消息发送失败: {:?}", e)
        }
        drop(socket_lock); // 显式释放锁
        
        // 等待一秒钟
        thread::sleep(Duration::from_secs(1));
    }
    
    // 主线程处理用户输入
    println!("你可以开始发送消息了，输入 'exit' 退出");
    loop {
        // 读取用户输入
        print!("> ");
        io::stdout().flush().unwrap();
        
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        
        // 去除尾部换行符
        let input = input.trim();
        
        if input == "exit" {
            // 发送关闭消息
            println!("退出命令，准备关闭连接");
            let mut socket_lock = socket.lock().unwrap();
            if let Err(e) = socket_lock.close(None) {
                println!("关闭连接错误: {:?}", e);
            }
            break;
        } else if !input.is_empty() {
            // 发送文本消息
            println!("发送消息: {}", input);
            let mut socket_lock = socket.lock().unwrap();
            if let Err(e) = socket_lock.write_message(Message::Text(input.to_string())) {
                println!("发送消息错误: {:?}", e);
                break;
            } else {
                println!("消息发送成功!");
            }
        }
    }
    
    // 等待线程结束
    let _ = receive_thread.join();
    
    println!("客户端已关闭");
} 