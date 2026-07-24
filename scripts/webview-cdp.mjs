const port = Number(process.argv[2]);
const expression = process.argv.slice(3).join(" ");

if (!Number.isInteger(port) || !expression) {
  console.error("Usage: node scripts/webview-cdp.mjs <port> <expression>");
  process.exit(2);
}

const targets = await fetch(`http://127.0.0.1:${port}/json/list`).then((response) => {
  if (!response.ok) throw new Error(`CDP target list returned HTTP ${response.status}`);
  return response.json();
});
const target = targets.find((candidate) => candidate.title === "EnvNexus AI") ?? targets[0];
if (!target?.webSocketDebuggerUrl) {
  throw new Error("EnvNexus AI WebView CDP target was not found");
}

const socket = new WebSocket(target.webSocketDebuggerUrl);
const requestId = 1;
const timeout = setTimeout(() => {
  socket.close();
  console.error("CDP evaluation timed out");
  process.exit(1);
}, 10_000);

socket.addEventListener("open", () => {
  socket.send(
    JSON.stringify({
      id: requestId,
      method: "Runtime.evaluate",
      params: {
        expression,
        awaitPromise: true,
        returnByValue: true,
      },
    }),
  );
});

socket.addEventListener("message", (event) => {
  const message = JSON.parse(String(event.data));
  if (message.id !== requestId) return;
  clearTimeout(timeout);
  socket.close();
  if (message.error || message.result?.exceptionDetails) {
    console.error(JSON.stringify(message.error ?? message.result.exceptionDetails));
    process.exit(1);
  }
  const value = message.result?.result?.value;
  process.stdout.write(JSON.stringify(value));
});

socket.addEventListener("error", () => {
  clearTimeout(timeout);
  console.error("Unable to connect to the EnvNexus AI WebView CDP target");
  process.exit(1);
});
