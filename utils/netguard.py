"""web_fetch must not become a way to reach the user's own machine or network
(localhost services, cloud metadata at 169.254.169.254, LAN devices)."""
from __future__ import annotations

import ipaddress
import socket
from urllib.parse import urlparse


def check_url(url: str, allow_private: bool = False) -> str | None:
    """Reason the URL must not be fetched, or None."""
    p = urlparse(url)
    if p.scheme not in ("http", "https") or not p.hostname:
        return "URL must be http:// or https://"
    if allow_private:
        return None
    host = p.hostname
    if host.lower() in {"localhost"} or host.lower().endswith((".localhost", ".local", ".internal")):
        return f"'{host}' is a local address"
    try:
        # ponytail: resolve-then-fetch leaves a DNS-rebinding window; pin the IP if this ever fronts untrusted input
        infos = socket.getaddrinfo(host, p.port or (443 if p.scheme == "https" else 80), proto=socket.IPPROTO_TCP)
    except socket.gaierror:
        return None  # unresolvable: the request itself will fail with a clear error
    for info in infos:
        ip = ipaddress.ip_address(info[4][0])
        if ip.is_private or ip.is_loopback or ip.is_link_local or ip.is_reserved or ip.is_multicast or ip.is_unspecified:
            return f"'{host}' resolves to a private or local address ({ip})"
    return None
