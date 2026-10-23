// Fake `codex app-server` for tests: NDJSON JSON-RPC 2.0 on stdio.
const rl = require('readline').createInterface({ input: process.stdin });
const send = (m) => process.stdout.write(JSON.stringify(m) + '\n');
let reads = 0, inited = false;
const limits = (pct) => ({
  limitId: 'codex', limitName: null, planType: 'pro', rateLimitReachedType: null, spendControlReached: false,
  primary: { usedPercent: pct, windowDurationMins: 300, resetsAt: 1900000000 },
  secondary: { usedPercent: 5, windowDurationMins: 10080, resetsAt: 1900500000 },
  credits: { hasCredits: false, unlimited: false, balance: null },
});
rl.on('line', (line) => {
  const m = JSON.parse(line);
  if (m.method === 'initialize') {
    if (!m.params || !m.params.clientInfo || m.params.clientInfo.name !== 'tandem') {
      return send({ id: m.id, error: { code: -32602, message: 'expected clientInfo.name tandem' } });
    }
    send({ method: 'remoteControl/status/changed', params: { status: 'disabled' } });
    return send({ id: m.id, result: { userAgent: 'fake/0', codexHome: '/tmp', platformFamily: 'unix', platformOs: 'macos' } });
  }
  if (m.method === 'initialized') { inited = true; return; }
  if (m.method === 'account/rateLimits/read') {
    if (!inited) return send({ id: m.id, error: { code: -32002, message: 'not initialized' } });
    reads += 1;
    const pct = reads === 1 ? 25 : 40;
    send({ id: m.id, result: { rateLimits: limits(pct), rateLimitsByLimitId: { codex: limits(pct) } } });
    if (reads === 1) {
      setTimeout(() => {
        send({ method: 'account/rateLimits/updated', params: { rateLimits: { limitId: 'codex', primary: { usedPercent: 30, windowDurationMins: 300, resetsAt: 1900000000 }, secondary: null, planType: null, rateLimitReachedType: null } } });
        send({ method: 'thread/tokenUsage/updated', params: { threadId: 't1', turnId: 'u1', tokenUsage: { modelContextWindow: 1000, last: { inputTokens: 100, cachedInputTokens: 0, outputTokens: 50, reasoningOutputTokens: 0, totalTokens: 150 }, total: { inputTokens: 300, cachedInputTokens: 0, outputTokens: 90, reasoningOutputTokens: 0, totalTokens: 390 } } } });
      }, 50);
    }
    return;
  }
  if (m.id !== undefined) send({ id: m.id, error: { code: -32601, message: 'method not found' } });
});
