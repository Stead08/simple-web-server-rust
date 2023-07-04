use std::collections::HashMap;
use std::{env, process};
use std::fs::File;
use std::io::{BufReader, Read, Write};
use anyhow::anyhow;
use log::{debug, error};
use mio::{Events, Token, Poll, Interest};
use mio::event::Event;
use regex::Regex;

const SERVER: Token = Token(0);
//ドキュメントルートのパス
const WEBROOT: &str = "/webroot";

struct WebServer {
    listening_socket: mio::net::TcpListener,
    connections: HashMap<usize, mio::net::TcpStream>, //サーバに接続されているクライアントを管理するハッシュテーブル
    next_connection_id: usize,
}

impl WebServer {
    /**
    * サーバの初期化
    */
    fn new(addr: &str) -> anyhow::Result<Self, anyhow::Error> {
        let address = addr.parse()?;
        let listening_socket = mio::net::TcpListener::bind(address)?;
        Ok(WebServer {
            listening_socket,
            connections: HashMap::new(),
            next_connection_id: 1,
        })
    }
    /**
     *
     */
    fn run(&mut self) -> Result<(), anyhow::Error> {
        let mut poll = Poll::new()?;
        // サーバーソケットの状態を監視対象に登録する
        poll.registry().register(
            &mut self.listening_socket,
            SERVER,
            Interest::READABLE
        )?;

        //イベントキュー
        let mut events = Events::with_capacity(1024);
        // HTTPのレスポンス用バッファ
        let mut response = Vec::new();

        loop {
            //現在のスレッドをブロックしてイベントを待つ。
            if let Err(e) = poll.poll(&mut events, None) {
                error!("{}", e);
                continue;
            }
            for event in &events {
                match event.token() {
                    SERVER => {
                        //　リスニングソケットの読み込み準備完了イベントが発生
                        let (stream, remote) = match self.listening_socket.accept() {
                            Ok(t) => t,
                            Err(e) => {
                                error!("{}", e);
                                continue;
                            }
                        };
                        debug!("Connection from {}", &remote);
                        //接続済みソケットを監視対象に登録
                        self.register_connection(&poll, stream).unwrap_or_else(|e| error!("{}", e));

                    }

                    Token(conn_id) => {
                        //　接続済みソケットでイベントが発生
                        self.http_handler(conn_id, event, &poll, &mut response)
                            .unwrap_or_else(|e| error!("{}", e));
                    }
                }
            }

        }


    }

    /**
    *　接続済みソケットを監視対象に登録する
    */
    fn register_connection(
        &mut self,
        poll: &Poll,
        mut stream: mio::net::TcpStream,
    ) -> anyhow::Result<(), anyhow::Error> {
        let token = Token(self.next_connection_id);
        poll.registry().register(&mut stream, token, Interest::READABLE)?;

        if self.connections.insert(self.next_connection_id, stream).is_some() {
            // HashMapは既存のキーで値が更新されると更新前の値を返す
            error!("Connection ID is already exist.");
        }
        self.next_connection_id += 1;
        Ok(())
    }

    /**
    * 接続済みソケットで発生したイベントのハンドラ
    */
    fn http_handler(
        &mut self,
        conn_id: usize,
        event: &Event,
        poll: &Poll,
        response: &mut Vec<u8>,
    ) -> anyhow::Result<(), anyhow::Error> {
        let stream = self
            .connections
            .get_mut(&conn_id)
            .ok_or_else(|| anyhow!("Invalid connection ID {}", conn_id))?;

        if event.is_readable() {
            //ソケットから読み込み可能
            debug!("readable conn_id: {}", conn_id);
            let mut buffer = [0u8; 1024];
            let nbytes = stream.read(&mut buffer)?;

            if nbytes != 0 {
                *response = make_response(&buffer[..nbytes])?;
                //書き込み操作の可否を監視対象に入れる
                poll.registry().reregister(stream, Token(conn_id), Interest::WRITABLE)?;
            } else {
                // 通信終了
                self.connections.remove(&conn_id);
            }
            Ok(())
        } else if event.is_writable() {
            //ソケットに書き込み可能
            debug!("writable conn_id: {}", conn_id);
            stream.write_all(response)?;
            self.connections.remove(&conn_id);
            Ok(())
        } else {
            Err(anyhow!("Undefined event"))
        }

    }

}

fn make_response(buffer: &[u8]) -> anyhow::Result<Vec<u8>, anyhow::Error> {
    //リクエストラインをパースする
    let http_pattern = Regex::new(r"(.*) (.*) HTTP/1.([0-1])\r\n.*")?;
    let Some(captures) = http_pattern.captures(std::str::from_utf8(buffer)?) else {
        //不正なリクエスト
        return create_msg_from_code(400, None);
    };

    let method = captures[1].to_string();
    let path = format!(
        "{}{}{}",
        env::current_dir()?.display(),
        WEBROOT,
        &captures[2]
    );
    let _version = captures[3].to_string();

    if method == "GET" {
        let Ok(file) = File::open(path) else {
            return create_msg_from_code(404, None);
        };
        let mut reader = BufReader::new(file);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        create_msg_from_code(200, Some(buf))
    } else {
        //サポートしていないHTTPメソッド
        create_msg_from_code(501, None)
    }

}

fn create_msg_from_code(
    status_code: u16,
    msg: Option<Vec<u8>>
) -> anyhow::Result<Vec<u8>, anyhow::Error> {
    match status_code {
        200 => {
            let mut header = "HTTP/1.0 200 OK\r\nserver: mio webserver\r\n\r\n"
                .to_string()
                .into_bytes();
            if let Some(mut msg) = msg {
                header.append(&mut msg);
            }
            Ok(header)
        },
        400 => Ok("HTTP/1.0 400 Bad Request\r\nServer: mio webserver\r\n\r\n"
            .to_string()
            .into_bytes()),
        404 => Ok("HTTP/1.0 404 Not Found\r\nServer: mio webserver\r\n\r\n"
            .to_string()
            .into_bytes()),
        501 => Ok("HTTP/1.0 501 Not Implemented\r\nServer: mio webserver\r\n\r\n"
            .to_string()
            .into_bytes()),
        _ => Err(anyhow!("Undefined status code."))
    }
}

fn main() {
    env::set_var("RUST_LOG", "debug");
    env_logger::init();
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        error!("wrong number of arguments");
        process::exit(1);
    }
    let mut server = WebServer::new(&args[1]).unwrap_or_else(|e| {
        error!("{}",e);
        panic!();
    });

    server.run().unwrap_or_else(|e| {
        error!("{}", e);
        panic!();
    })
}
