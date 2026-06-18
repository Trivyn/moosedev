"""Minimal async helper to call one tool on a stdio MCP server (e.g. `moosedev --connect`)."""
import os
from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client


async def call_tool(command: str, args: list[str], env: dict, tool: str, arguments: dict) -> str:
    """Spawn the MCP server, call `tool`, return the joined text content."""
    params = StdioServerParameters(command=command, args=args, env={**os.environ, **env})
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            result = await session.call_tool(tool, arguments)
            texts = [c.text for c in result.content if getattr(c, "type", None) == "text"]
            return "\n".join(texts)
