use crate::session::ConnectParams;
use anyhow::{Context, Result, ensure};
use base64::Engine;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, lookup_host},
};
use tokio_socks::tcp::{Socks4Stream, Socks5Stream};

pub(crate) async fn connect(p: &ConnectParams, sink: &crate::session::EventSink) -> Result<TcpStream> {
    if p.proxy.is_empty() {
        return TcpStream::connect((p.host.as_str(), p.port)).await.map_err(|e| crate::session::connect_error(&p.host, p.port, e));
    }
    let url = url::Url::parse(&p.proxy).context("Invalid proxy URL")?;
    ensure!(url.username().is_empty() && url.password().is_none(), "Enter proxy username separately");
    let host = url.host_str().context("Missing proxy host")?.trim_matches(['[', ']']);
    let port = url.port().unwrap_or(if url.scheme() == "http" { 80 } else { 1080 });
    let password = if !p.proxy_username.is_empty() && matches!(url.scheme(), "http" | "socks5") {
        let fields = vec![crate::PromptField { label: "Proxy password".into(), echo: false }];
        let answers = sink
            .prompt(format!("Proxy login: {} at {host}:{port}", p.proxy_username), String::new(), fields, None, false)
            .await
            .context("Proxy authentication cancelled")?;
        Some(zeroize::Zeroizing::new(answers.into_iter().next().context("Missing proxy password")?))
    } else {
        None
    };
    let target = if p.proxy_remote_dns {
        p.host.clone()
    } else {
        lookup_host((p.host.as_str(), p.port))
            .await?
            .find(|a| url.scheme() != "socks4" || a.is_ipv4())
            .context("No suitable target address")?
            .ip()
            .to_string()
    };
    let socket = TcpStream::connect((host, port)).await?;
    match url.scheme() {
        "socks4" => Ok(if p.proxy_username.is_empty() {
            Socks4Stream::connect_with_socket(socket, (target.as_str(), p.port)).await?
        } else {
            Socks4Stream::connect_with_userid_and_socket(socket, (target.as_str(), p.port), &p.proxy_username).await?
        }
        .into_inner()),
        "socks5" => Ok(if let Some(pass) = password {
            Socks5Stream::connect_with_password_and_socket(socket, (target.as_str(), p.port), &p.proxy_username, &pass).await?
        } else {
            Socks5Stream::connect_with_socket(socket, (target.as_str(), p.port)).await?
        }
        .into_inner()),
        "http" => {
            ensure!(
                !target.is_empty() && !target.bytes().any(|b| b <= 32 || b >= 127 || b"/?#@".contains(&b)),
                "Invalid CONNECT destination"
            );
            let authority = if target.contains(':') { format!("[{target}]:{}", p.port) } else { format!("{target}:{}", p.port) };
            let mut socket = socket;
            let mut request = zeroize::Zeroizing::new(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n"));
            if let Some(pass) = password {
                let auth = zeroize::Zeroizing::new(format!("{}:{}", p.proxy_username, pass.as_str()));
                request.push_str("Proxy-Authorization: Basic ");
                request.push_str(&base64::engine::general_purpose::STANDARD.encode(auth.as_bytes()));
                request.push_str("\r\n");
            }
            request.push_str("\r\n");
            socket.write_all(request.as_bytes()).await?;
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                ensure!(header.len() < 16384, "Proxy response headers too large");
                header.push(socket.read_u8().await?);
            }
            let mut status = header.split(|&b| b == b'\n').next().unwrap_or_default().split(|&b| b == b' ');
            ensure!(
                matches!(status.next(), Some(b"HTTP/1.0" | b"HTTP/1.1"))
                    && status
                        .next()
                        .and_then(|s| std::str::from_utf8(s).ok()?.trim().parse::<u16>().ok())
                        .is_some_and(|v| (200..300).contains(&v)),
                "HTTP proxy rejected CONNECT"
            );
            Ok(socket)
        }
        _ => anyhow::bail!("Expected http, socks4 or socks5 proxy"),
    }
}
