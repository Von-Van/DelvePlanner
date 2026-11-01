"""Local model access (handoff section 4.3).

`ModelClient` is the interface every agent uses. `OllamaModelClient` talks to an Ollama server on a
loopback address only: it refuses any other host, ignores proxy environment variables, and refuses
HTTP redirects, so a request can never leave the machine. In the desktop app the same interface will
be served by the Rust model broker over the sidecar link (piece D); this client is for development
and the benchmark harness.
"""

from __future__ import annotations

import ipaddress
import json
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from typing import Any, Protocol


class ModelError(Exception):
    """The model could not produce a response (unreachable, bad reply, refused endpoint)."""


class NonLoopbackEndpoint(ModelError):
    """The configured endpoint is not a loopback address. Remote inference is never allowed."""


@dataclass(frozen=True)
class ModelRequest:
    messages: list[dict[str, str]]
    # JSON Schema for structured output, the string "json", or None for free text.
    format: dict[str, Any] | str | None = None
    options: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class ModelResponse:
    content: str
    model: str
    digest: str | None
    options: dict[str, Any]
    latency_ms: int
    prompt_tokens: int = 0
    completion_tokens: int = 0
    runtime_version: str | None = None

    def metadata(self) -> dict[str, Any]:
        """Everything about the call except the text, for routine traces."""
        return {
            "model": self.model,
            "digest": self.digest,
            "options": dict(self.options),
            "latencyMs": self.latency_ms,
            "promptTokens": self.prompt_tokens,
            "completionTokens": self.completion_tokens,
            "runtimeVersion": self.runtime_version,
        }


class ModelClient(Protocol):
    def complete(self, request: ModelRequest) -> ModelResponse: ...

    def describe(self) -> dict[str, Any]:
        """Model name, digest, runtime version and default options, for run records."""
        ...


def require_loopback(base_url: str) -> str:
    """Return a normalised base URL, or raise if it is not plain http on a loopback IP literal.

    Host names (even "localhost") are rejected because name resolution can be redirected; only
    127.0.0.0/8 and ::1 are accepted.
    """
    parsed = urllib.parse.urlsplit(base_url)
    if parsed.scheme != "http":
        raise NonLoopbackEndpoint(f"only http on loopback is allowed, got scheme {parsed.scheme!r}")
    if parsed.username or parsed.password:
        raise NonLoopbackEndpoint("credentials in the model URL are not allowed")
    host = parsed.hostname or ""
    try:
        address = ipaddress.ip_address(host)
    except ValueError:
        raise NonLoopbackEndpoint(f"model host must be a loopback IP address, got {host!r}") from None
    if not address.is_loopback:
        raise NonLoopbackEndpoint(f"model host {host} is not a loopback address")
    if parsed.port is None:
        raise NonLoopbackEndpoint("model URL must include an explicit port")
    netloc = f"[{host}]:{parsed.port}" if address.version == 6 else f"{host}:{parsed.port}"
    return f"http://{netloc}"


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: ANN001
        raise ModelError(f"model endpoint tried to redirect to {newurl}; refused")


def _loopback_opener() -> urllib.request.OpenerDirector:
    # An empty ProxyHandler stops urllib from honouring HTTP(S)_PROXY, which would otherwise route
    # "loopback" traffic through a remote proxy.
    return urllib.request.build_opener(urllib.request.ProxyHandler({}), _NoRedirect())


class OllamaModelClient:
    """Calls Ollama's /api/chat on a loopback port and records model, digest, options and latency."""

    def __init__(
        self,
        base_url: str = "http://127.0.0.1:11434",
        model: str = "qwen3:8b",
        options: dict[str, Any] | None = None,
        timeout_seconds: float = 120.0,
        think: bool | None = False,
    ) -> None:
        self.base_url = require_loopback(base_url)
        self.model = model
        # Deterministic-leaning defaults; benchmark configs override these explicitly.
        self.options = {"temperature": 0, "seed": 7, **(options or {})}
        self.timeout_seconds = timeout_seconds
        self.think = think
        self._opener = _loopback_opener()
        self._digest: str | None = None
        self._version: str | None = None
        self._described = False

    # -- HTTP -----------------------------------------------------------------------------------

    def _request(self, path: str, body: dict[str, Any] | None = None) -> dict[str, Any]:
        url = self.base_url + path
        data = None if body is None else json.dumps(body).encode("utf-8")
        request = urllib.request.Request(url, data=data, method="GET" if body is None else "POST")
        request.add_header("Content-Type", "application/json")
        try:
            with self._opener.open(request, timeout=self.timeout_seconds) as response:
                return json.loads(response.read().decode("utf-8"))
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", "replace")[:500]
            raise ModelError(f"Ollama returned HTTP {error.code}: {detail}") from error
        except (urllib.error.URLError, TimeoutError, OSError) as error:
            raise ModelError(f"Ollama is not reachable at {self.base_url}: {error}") from error
        except json.JSONDecodeError as error:
            raise ModelError("Ollama returned a non-JSON response") from error

    # -- metadata -------------------------------------------------------------------------------

    def _load_metadata(self) -> None:
        if self._described:
            return
        self._described = True
        try:
            self._version = self._request("/api/version").get("version")
        except ModelError:
            self._version = None
        try:
            for entry in self._request("/api/tags").get("models", []):
                if entry.get("name") == self.model or entry.get("model") == self.model:
                    self._digest = entry.get("digest")
                    break
        except ModelError:
            self._digest = None

    def describe(self) -> dict[str, Any]:
        self._load_metadata()
        return {
            "provider": "ollama",
            "endpoint": self.base_url,
            "model": self.model,
            "digest": self._digest,
            "runtimeVersion": self._version,
            "options": dict(self.options),
        }

    # -- inference ------------------------------------------------------------------------------

    def complete(self, request: ModelRequest) -> ModelResponse:
        self._load_metadata()
        options = {**self.options, **request.options}
        body: dict[str, Any] = {
            "model": self.model,
            "messages": request.messages,
            "stream": False,
            "options": options,
        }
        if request.format is not None:
            body["format"] = request.format
        if self.think is not None:
            body["think"] = self.think
        started = time.monotonic()
        reply = self._request("/api/chat", body)
        latency_ms = int((time.monotonic() - started) * 1000)
        message = reply.get("message") or {}
        content = message.get("content")
        if not isinstance(content, str):
            raise ModelError("Ollama reply had no message content")
        return ModelResponse(
            content=content,
            model=str(reply.get("model") or self.model),
            digest=self._digest,
            options=options,
            latency_ms=latency_ms,
            prompt_tokens=int(reply.get("prompt_eval_count") or 0),
            completion_tokens=int(reply.get("eval_count") or 0),
            runtime_version=self._version,
        )
