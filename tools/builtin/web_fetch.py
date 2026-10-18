from urllib.parse import urlparse

import httpx
from utils.netguard import check_url
from tools.base import Tool, ToolInvocation, ToolKind, ToolResult
from pydantic import BaseModel, Field


class WebFetchParams(BaseModel):
    url: str = Field(..., description="URL to fetch (must be http:// or https://)")
    timeout: int = Field(
        30,
        ge=5,
        le=120,
        description="Request timeout in seconds (default: 120)",
    )


class WebFetchTool(Tool):
    name = "web_fetch"
    description = "Fetch a URL and return the body as text. Use it for URLs the user gave or that a search returned; do not guess URLs from memory. It cannot reach pages behind a login: use `gh` or an authenticated tool for those."
    kind = ToolKind.NETWORK
    schema = WebFetchParams

    async def execute(self, invocation: ToolInvocation) -> ToolResult:
        params = WebFetchParams(**invocation.params)

        parsed = urlparse(params.url)
        if not parsed.scheme or parsed.scheme not in ("http", "https"):
            return ToolResult.error_result(f"Url must be http:// or https://")

        url = params.url
        try:
            async with httpx.AsyncClient(timeout=httpx.Timeout(params.timeout), follow_redirects=False) as client:
                for _hop in range(6):
                    # every hop is checked, so a public page cannot redirect us to localhost or cloud metadata
                    if why := check_url(url, self.config.allow_private_network):
                        return ToolResult.error_result(f"Blocked: {why}")
                    response = await client.get(url)
                    if response.is_redirect and response.headers.get("location"):
                        url = str(httpx.URL(url).join(response.headers["location"]))
                        continue
                    break
                else:
                    return ToolResult.error_result("Too many redirects")
                response.raise_for_status()
                text = response.text
        except httpx.HTTPStatusError as e:
            return ToolResult.error_result(
                f"HTTP {e.response.status_code}: {e.response.reason_phrase}",
            )
        except Exception as e:
            return ToolResult.error_result(f"Request failed: {e}")

        if len(text) > 100 * 1024:
            text = text[: 100 * 1024] + "\n... [content truncated]"

        return ToolResult.success_result(
            text,
            metadata={
                "status_code": response.status_code,
                "content_length": len(response.content),
            },
        )
