use std::net::{TcpListener, TcpStream};
use std::thread::spawn;
use std::io::{Read, Write};
use tungstenite::{accept, Message};

fn main() {
    // 创建TCP监听器，绑定到本地9001端口
    let server = TcpListener::bind("127.0.0.1:9001").unwrap();
    println!("WebSocket服务器已启动，监听端口9001");

    // 处理连接
    for stream in server.incoming() {
        spawn(move || {
            let stream = stream.unwrap();
            
            // 获取客户端IP地址
            let client_ip = match stream.peer_addr() {
                Ok(addr) => addr.to_string(),
                Err(_) => "未知IP地址".to_string()
            };
            
            println!("收到来自 {} 的新连接，尝试进行WebSocket握手...", client_ip);
            
            // 直接查看原始TCP数据
            let mut peek_buffer = [0u8; 1024];
            let mut stream_clone = stream.try_clone().expect("无法克隆流");
            match stream_clone.peek(&mut peek_buffer) {
                Ok(n) if n > 0 => {
                    println!("接收到握手数据: {} 字节", n);
                    if let Ok(s) = std::str::from_utf8(&peek_buffer[..n]) {
                        println!("握手数据预览: {}", s.chars().take(100).collect::<String>());
                    }
                },
                Ok(0) => println!("连接已关闭"),
                Ok(n) => println!("接收到 {} 字节数据", n),
                Err(e) => println!("预览数据错误: {:?}", e),
            }
            
            // 接受WebSocket连接
            let mut websocket = match accept(stream) {
                Ok(ws) => {
                    println!("WebSocket握手成功，客户端 {} 已连接", client_ip);
                    ws
                },
                Err(e) => {
                    println!("与客户端 {} 的WebSocket握手失败: {:?}", client_ip, e);
                    return;
                }
            };
            
            // 处理消息
            loop {
                println!("等待客户端 {} 的消息...", client_ip);
                
                // 尝试直接访问底层流
                {
                    let stream = websocket.get_mut();
                    let mut peek_buffer = [0u8; 1024];
                    match stream.peek(&mut peek_buffer) {
                        Ok(n) if n > 0 => {
                            println!("TCP层收到来自 {} 的数据: {} 字节", client_ip, n);
                            println!("原始数据(十六进制): {:02X?}", &peek_buffer[..std::cmp::min(n, 50)]);
                        },
                        Ok(0) => println!("TCP连接已关闭"),
                        Ok(n) => println!("TCP层收到 {} 字节数据", n),
                        Err(e) => println!("TCP数据预览错误: {:?}", e),
                    }
                }
                
                // 读取WebSocket消息
                let msg = match websocket.read_message() {
                    Ok(msg) => {
                        println!("成功读取到来自 {} 的一条消息，消息类型: {:?}", client_ip, msg);
                        msg
                    },
                    Err(e) => {
                        println!("读取来自 {} 的消息错误: {:?}", client_ip, e);
                        break;
                    }
                };

                // 使用模式匹配明确处理不同类型的消息
                match msg {
                    Message::Text(text) => {
                        println!("====================");
                        println!("服务器收到来自 {} 的文本消息: {}", client_ip, text);
                        println!("====================");
                        
                        // 发送响应消息
                        let response = format!("服务器收到来自 {} 的消息: {}", client_ip, text);
                        match websocket.write_message(Message::Text(response)) {
                            Ok(_) => {
                                println!("服务器已回复 {} 的消息", client_ip);
                            },
                            Err(e) => {
                                println!("发送消息给 {} 时出错: {:?}", client_ip, e);
                                break;
                            }
                        }
                    },
                    Message::Binary(data) => {
                        println!("服务器收到来自 {} 的二进制消息，长度: {} 字节", client_ip, data.len());
                    },
                    Message::Ping(data) => {
                        println!("服务器收到来自 {} 的Ping", client_ip);
                        // 自动回复Pong
                        if let Err(e) = websocket.write_message(Message::Pong(data)) {
                            println!("发送Pong给 {} 时出错: {:?}", client_ip, e);
                        }
                    },
                    Message::Pong(_) => {
                        println!("服务器收到来自 {} 的Pong", client_ip);
                    },
                    Message::Close(_) => {
                        println!("客户端 {} 请求关闭连接", client_ip);
                        break;
                    },
                    Message::Frame(_) => {
                        println!("服务器收到来自 {} 的原始帧", client_ip);
                    }
                }
            }

            println!("客户端 {} 的连接已关闭", client_ip);
        });
    }
}
