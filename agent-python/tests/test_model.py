from __future__ import annotations

import pytest
from conftest import FakeOllama

from delveops_agent.model import ModelError, ModelRequest, NonLoopbackEndpoint, OllamaModelClient, require_loopback


@pytest.mark.parametrize(
    "url, expected",
    [
        ("http://127.0.0.1:11434", "http://127.0.0.1:11434"),
        ("http://127.0.0.1:11434/", "http://127.0.0.1:11434"),
        ("http://127.5.6.7:9", "http://127.5.6.7:9"),
        ("http://[::1]:11434", "http://[::1]:11434"),
    ],
)
def test_loopback_urls_are_accepted(url, expected):
    assert require_loopback(url) == expected


@pytest.mark.parametrize(
    "url",
    [
        "http://localhost:11434",  # names can be re-pointed; only IP literals
        "http://10.0.0.5:11434",
        "http://192.168.1.2:11434",
        "http://0.0.0.0:11434",
        "https://127.0.0.1:11434",
        "http://127.0.0.1",  # no port
        "http://user:pw@127.0.0.1:11434",
        "http://api.example.com:443",
        "ftp://127.0.0.1:21",
    ],
)
def test_non_loopback_urls_are_refused(url):
    with pytest.raises(NonLoopbackEndpoint):
        require_loopback(url)


def test_client_refuses_remote_endpoint_at_construction():
    with pytest.raises(NonLoopbackEndpoint):
        OllamaModelClient("http://203.0.113.9:11434")


def test_complete_records_model_digest_options_and_latency(ollama):
    client = OllamaModelClient(ollama, model="qwen3:8b", options={"num_ctx": 8192})
    response = client.complete(
        ModelRequest(messages=[{"role": "user", "content": "hello"}], format="json", options={"temperature": 0.2})
    )
    assert response.content == '{"action": "answer", "answer": "hi"}'
    assert response.model == "qwen3:8b"
    assert response.digest == "sha256:abc123"
    assert response.runtime_version == "0.12.6"
    assert response.options == {"temperature": 0.2, "seed": 7, "num_ctx": 8192}
    assert response.prompt_tokens == 42 and response.completion_tokens == 7
    assert response.latency_ms >= 0
    sent = FakeOllama.chat_bodies[0]
    assert sent["stream"] is False and sent["format"] == "json" and sent["think"] is False
    assert sent["options"]["num_ctx"] == 8192
    assert "content" not in response.metadata()


def test_describe_reports_run_metadata(ollama):
    info = OllamaModelClient(ollama).describe()
    assert info["digest"] == "sha256:abc123"
    assert info["runtimeVersion"] == "0.12.6"
    assert info["endpoint"] == ollama


def test_proxy_environment_is_ignored(ollama, monkeypatch):
    # If proxies were honoured, this would send the request to a dead remote proxy and fail.
    monkeypatch.setenv("HTTP_PROXY", "http://203.0.113.1:3128")
    monkeypatch.setenv("http_proxy", "http://203.0.113.1:3128")
    monkeypatch.setenv("NO_PROXY", "")
    client = OllamaModelClient(ollama, timeout_seconds=5)
    assert client.complete(ModelRequest(messages=[{"role": "user", "content": "x"}])).content


def test_redirects_are_refused(ollama):
    FakeOllama.redirect = True
    client = OllamaModelClient(ollama, timeout_seconds=5)
    with pytest.raises(ModelError, match="redirect"):
        client.complete(ModelRequest(messages=[{"role": "user", "content": "x"}]))


def test_http_errors_become_model_errors(ollama):
    client = OllamaModelClient(ollama, model="missing", timeout_seconds=5)
    with pytest.raises(ModelError, match="HTTP 404"):
        client.complete(ModelRequest(messages=[{"role": "user", "content": "x"}]))


def test_unreachable_server_is_a_model_error():
    client = OllamaModelClient("http://127.0.0.1:9", timeout_seconds=2)
    with pytest.raises(ModelError):
        client.complete(ModelRequest(messages=[{"role": "user", "content": "x"}]))
    assert client.describe()["digest"] is None
