let lastFetch = Date.now();

function fmt(n) {
  if (n >= 1e6) return (n/1e6).toFixed(1)+'M';
  if (n >= 1e3) return (n/1e3).toFixed(1)+'K';
  return String(n);
}

function fmtTime(s) {
  if (s >= 3600) return (s/3600).toFixed(1)+'h';
  if (s >= 60) return (s/60).toFixed(1)+'m';
  return s.toFixed(0)+'s';
}

function ago(iso) {
  if (!iso) return '-';
  const d = (Date.now() - new Date(iso).getTime()) / 1000;
  if (d < 5) return 'just now';
  if (d < 60) return Math.floor(d)+'s ago';
  if (d < 3600) return Math.floor(d/60)+'m ago';
  return Math.floor(d/3600)+'h ago';
}

function esc(s) { const d=document.createElement('div'); d.textContent=s; return d.innerHTML; }

function renderRunning(items, mode) {
  const el = document.getElementById('runningContent');
  document.getElementById('runningBadge').textContent = items.length;
  if (!items.length) {
    el.innerHTML = '<div class="empty-state"><div class="icon">No running sessions</div><p>Waiting for Linear issues in active states.<br>Move an issue to Todo or In Progress to start.</p></div>';
    return;
  }
  let h = '<div class="table-responsive"><table><thead><tr><th>Issue</th><th>State</th>';
  if (mode === 'distributed') h += '<th>Worker</th>';
  h += '<th class="col-session">Session</th><th>Turns</th><th>Last Event</th><th>Last Message</th><th class="col-tokens">Tokens</th><th>Started</th></tr></thead><tbody>';
  for (const r of items) {
    const msg = r.last_message || '';
    const msgPreview = msg.length > 80 ? esc(msg.slice(0, 80)) + '&hellip;' : (msg ? esc(msg) : '-');
    h += '<tr onclick="openLogPanel(\''+esc(r.issue_identifier)+'\')" title="Click to view live logs">';
    h += '<td class="mono"><strong>'+esc(r.issue_identifier)+'</strong></td>';
    h += '<td><span class="state-badge active">'+esc(r.state)+'</span></td>';
    if (mode === 'distributed') h += '<td class="mono">'+(r.worker_id ? esc(r.worker_id) : '-')+'</td>';
    h += '<td class="mono col-session">'+(r.session_id ? esc(r.session_id).slice(0,12) : '-')+'</td>';
    h += '<td>'+r.turn_count+'</td>';
    h += '<td>'+(r.last_event ? esc(r.last_event) : '-')+'</td>';
    h += '<td style="max-width:300px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;" title="'+(msg ? esc(msg) : '')+'">'+msgPreview+'</td>';
    h += '<td class="mono col-tokens">'+fmt(r.total_tokens)+'</td>';
    h += '<td>'+ago(r.started_at)+'</td>';
    h += '</tr>';
  }
  h += '</tbody></table></div>';
  el.innerHTML = h;
}

function renderWorkers(items) {
  const el = document.getElementById('workersContent');
  document.getElementById('workersBadge').textContent = items.length;
  if (!items.length) {
    el.innerHTML = '<div class="empty-state"><div class="icon">No workers connected</div><p>Workers will appear here when they register with the orchestrator.</p></div>';
    return;
  }
  let h = '<div class="table-responsive"><table><thead><tr><th class="col-session">Worker ID</th><th>Status</th><th>Active Jobs</th><th>Max Jobs</th><th>Last Heartbeat</th><th>Registered</th></tr></thead><tbody>';
  for (const w of items) {
    h += '<tr>';
    h += '<td class="mono col-session"><strong>'+esc(w.worker_id)+'</strong></td>';
    h += '<td><span class="state-badge '+esc(w.status)+'">'+esc(w.status)+'</span></td>';
    h += '<td class="mono">'+w.active_jobs.map(j => esc(j)).join(', ')+'</td>';
    h += '<td>'+w.max_concurrent_jobs+'</td>';
    h += '<td>'+ago(w.last_heartbeat)+'</td>';
    h += '<td>'+ago(w.registered_at)+'</td>';
    h += '</tr>';
  }
  h += '</tbody></table></div>';
  el.innerHTML = h;
}

function renderPendingJobs(items) {
  const el = document.getElementById('pendingJobsContent');
  document.getElementById('pendingJobsBadge').textContent = items.length;
  if (!items.length) {
    el.innerHTML = '<div class="empty-state"><div class="icon">No pending jobs</div><p>Jobs waiting to be claimed by workers will appear here.</p></div>';
    return;
  }
  let h = '<div class="table-responsive"><table><thead><tr><th>Issue</th><th>Attempt</th><th>Prompt Preview</th></tr></thead><tbody>';
  for (const j of items) {
    h += '<tr>';
    h += '<td class="mono"><strong>'+esc(j.issue_identifier)+'</strong></td>';
    h += '<td>'+(j.attempt != null ? '#'+j.attempt : '-')+'</td>';
    h += '<td>'+esc(j.prompt_preview)+'</td>';
    h += '</tr>';
  }
  h += '</tbody></table></div>';
  el.innerHTML = h;
}

function renderRetrying(items) {
  const el = document.getElementById('retryContent');
  document.getElementById('retryBadge').textContent = items.length;
  if (!items.length) {
    el.innerHTML = '<div class="empty-state"><div class="icon">No retries pending</div><p>Failed or timed-out sessions will appear here with their retry schedule.</p></div>';
    return;
  }
  let h = '<div class="table-responsive"><table><thead><tr><th>Issue</th><th>Attempt</th><th>Retry In</th><th>Error</th><th></th></tr></thead><tbody>';
  for (const r of items) {
    const dueIn = Math.max(0, r.due_at_ms - Date.now());
    const dueStr = dueIn > 0 ? fmtTime(dueIn/1000) : 'now';
    h += '<tr>';
    h += '<td class="mono"><strong>'+esc(r.identifier)+'</strong></td>';
    h += '<td><span class="state-badge retry">#'+r.attempt+'</span></td>';
    h += '<td>'+dueStr+'</td>';
    h += '<td>'+(r.error ? esc(r.error) : '-')+'</td>';
    h += '<td><button class="btn btn-danger" onclick="removeRetry(\''+esc(r.identifier)+'\')">Remove</button></td>';
    h += '</tr>';
  }
  h += '</tbody></table></div>';
  el.innerHTML = h;
}

function renderWaiting(items) {
  const el = document.getElementById('waitingContent');
  document.getElementById('waitingBadge').textContent = items.length;
  if (!items.length) {
    el.innerHTML = '<div class="empty-state"><div class="icon">No PRs waiting</div><p>Issues waiting for PR review or merge will appear here.</p></div>';
    return;
  }
  let h = '<div class="table-responsive"><table><thead><tr><th>Issue</th><th>PR</th><th>Branch</th><th>Waiting</th></tr></thead><tbody>';
  for (const w of items) {
    h += '<tr>';
    h += '<td class="mono"><strong>'+esc(w.identifier)+'</strong></td>';
    h += '<td><span class="state-badge active">#'+w.pr_number+'</span></td>';
    h += '<td class="mono">'+esc(w.branch)+'</td>';
    h += '<td>'+ago(w.started_waiting_at)+'</td>';
    h += '</tr>';
  }
  h += '</tbody></table></div>';
  el.innerHTML = h;
}

let completedVisible = false;
function toggleCompleted() {
  completedVisible = !completedVisible;
  document.getElementById('completedContent').style.display = completedVisible ? '' : 'none';
  document.getElementById('completedToggle').textContent = completedVisible ? 'Hide' : 'Show';
}

function fmtDuration(s) {
  if (s >= 3600) return (s/3600).toFixed(1)+'h';
  if (s >= 60) return (s/60).toFixed(1)+'m';
  return s.toFixed(0)+'s';
}

function renderCompleted(items) {
  const el = document.getElementById('completedContent');
  document.getElementById('completedBadge').textContent = items.length;
  if (!items.length) {
    el.innerHTML = '<div class="empty-state"><div class="icon">No completed runs</div><p>Finished agent sessions will appear here.</p></div>';
    return;
  }
  let h = '<div class="table-responsive"><table><thead><tr><th>Issue</th><th>Outcome</th><th>Duration</th><th class="col-tokens">Tokens</th><th>Turns</th><th>Completed</th></tr></thead><tbody>';
  for (const r of items) {
    h += '<tr>';
    h += '<td class="mono"><strong>'+esc(r.issue_identifier)+'</strong></td>';
    h += '<td><span class="state-badge '+esc(r.outcome)+'">'+esc(r.outcome)+'</span></td>';
    h += '<td>'+fmtDuration(r.duration_seconds)+'</td>';
    h += '<td class="mono col-tokens">'+fmt(r.total_tokens)+'</td>';
    h += '<td>'+r.turn_count+'</td>';
    h += '<td>'+ago(r.completed_at)+'</td>';
    h += '</tr>';
  }
  h += '</tbody></table></div>';
  el.innerHTML = h;
}

function renderRateLimits(rl) {
  const el = document.getElementById('rateLimitContent');
  if (!rl) {
    el.innerHTML = '<div class="empty-state"><p>No rate limit data</p></div>';
    return;
  }
  el.innerHTML = '<pre>'+esc(JSON.stringify(rl, null, 2))+'</pre>';
}

function update(data) {
  const t = data.agent_totals || {};
  const mode = data.deployment_mode || 'local';
  document.getElementById('countRunning').textContent = (data.counts||{}).running || 0;
  document.getElementById('countRetrying').textContent = (data.counts||{}).retrying || 0;
  document.getElementById('countWaiting').textContent = (data.counts||{}).waiting || 0;
  document.getElementById('countCompleted').textContent = (data.counts||{}).completed || 0;
  document.getElementById('totalTokens').textContent = fmt(t.total_tokens || 0);
  document.getElementById('inputTokens').textContent = fmt(t.input_tokens || 0) + ' in';
  document.getElementById('outputTokens').textContent = fmt(t.output_tokens || 0) + ' out';
  document.getElementById('runtime').textContent = fmtTime(t.seconds_running || 0);
  renderRunning(data.running || [], mode);
  renderRetrying(data.retrying || []);
  renderWaiting(data.waiting || []);
  renderCompleted(data.completed || []);
  renderRateLimits(data.rate_limits);
  if (mode === 'distributed') {
    document.getElementById('workersSection').style.display = '';
    document.getElementById('pendingJobsSection').style.display = '';
    renderWorkers(data.workers || []);
    renderPendingJobs(data.pending_jobs || []);
  } else {
    document.getElementById('workersSection').style.display = 'none';
    document.getElementById('pendingJobsSection').style.display = 'none';
  }
  lastFetch = Date.now();
}

let stateEvtSource = null;
let pollTimer = null;

function startSSE() {
  stateEvtSource = new EventSource('/api/v1/state/stream');
  stateEvtSource.onmessage = function(e) {
    try {
      const data = JSON.parse(e.data);
      update(data);
      document.getElementById('statusDot').style.background = 'var(--green)';
      document.getElementById('statusText').textContent = 'Live (SSE)';
    } catch(err) { /* ignore parse errors */ }
  };
  stateEvtSource.onerror = function() {
    stateEvtSource.close();
    stateEvtSource = null;
    startPolling();
  };
  if (pollTimer) { clearInterval(pollTimer); pollTimer = null; }
}

async function poll() {
  try {
    const r = await fetch('/api/v1/state');
    if (r.ok) {
      update(await r.json());
      document.getElementById('statusDot').style.background = 'var(--green)';
      document.getElementById('statusText').textContent = 'Polling (fallback)';
    }
  } catch(e) {
    document.getElementById('statusDot').style.background = 'var(--red)';
    document.getElementById('statusText').textContent = 'Disconnected';
  }
}

function startPolling() {
  document.getElementById('statusText').textContent = 'Polling (fallback)';
  poll();
  pollTimer = setInterval(poll, 3000);
  setTimeout(function() { if (!stateEvtSource) startSSE(); }, 10000);
}

async function triggerRefresh() {
  const btn = document.getElementById('refreshBtn');
  btn.textContent = 'Refreshing...';
  btn.disabled = true;
  try {
    await fetch('/api/v1/refresh', { method: 'POST' });
    await new Promise(r => setTimeout(r, 500));
    await poll();
  } catch(e) {}
  btn.textContent = 'Refresh Now';
  btn.disabled = false;
}

async function removeRetry(identifier) {
  if (!confirm('Remove ' + identifier + ' from retry queue? It will stop retrying.')) return;
  try {
    const r = await fetch('/api/v1/' + encodeURIComponent(identifier) + '/remove-retry', { method: 'POST' });
    if (r.ok) {
      await poll();
    } else {
      const data = await r.json().catch(() => ({}));
      alert('Failed to remove: ' + (data.error?.message || r.statusText));
    }
  } catch(e) {
    alert('Failed to remove: ' + e.message);
  }
}

// Update relative timestamps every second
setInterval(() => {
  const s = Math.floor((Date.now() - lastFetch) / 1000);
  document.getElementById('lastUpdate').textContent = s < 3 ? 'Updated just now' : 'Updated '+s+'s ago';
}, 1000);

// ── Live Log Panel ──────────────────────────────────────────────
let currentLogIssue = null;
let currentEventSource = null;

function openLogPanel(identifier) {
  if (currentLogIssue === identifier) { closeLogPanel(); return; }
  closeLogPanel();
  currentLogIssue = identifier;
  const panel = document.getElementById('logPanel');
  const title = document.getElementById('logPanelTitle');
  const body = document.getElementById('logPanelBody');
  title.textContent = 'Event Log — ' + identifier;
  body.innerHTML = '';
  panel.classList.add('open');
  currentEventSource = new EventSource('/api/v1/' + encodeURIComponent(identifier) + '/stream');
  currentEventSource.onmessage = function(e) { appendLogEntry(JSON.parse(e.data)); };
  currentEventSource.onerror = function() { appendLogEntry({event_type:'notification',message:'Stream disconnected',seq:0,timestamp:new Date().toISOString()}); };
}

function closeLogPanel() {
  if (currentEventSource) { currentEventSource.close(); currentEventSource = null; }
  currentLogIssue = null;
  document.getElementById('logPanel').classList.remove('open');
}

function appendLogEntry(entry) {
  const body = document.getElementById('logPanelBody');
  const div = document.createElement('div');
  div.className = 'log-entry';
  const ts = entry.timestamp ? ago(entry.timestamp) : '';
  const badgeClass = entry.event_type || 'notification';
  let parts = '<span class="log-ts">'+esc(ts)+'</span>';
  parts += '<span class="log-badge '+esc(badgeClass)+'">'+esc(entry.event_type||'')+'</span>';
  if (entry.message) parts += '<span class="log-msg">'+esc(entry.message)+'</span>';
  else if (!entry.tokens) return;
  if (entry.tokens) parts += '<span class="log-tokens">'+fmt(entry.tokens.total_tokens||0)+' tok</span>';
  div.innerHTML = parts;
  body.appendChild(div);
  body.scrollTop = body.scrollHeight;
}

// Initial render from server-embedded data
update(INITIAL);
// Start SSE (falls back to polling on error)
startSSE();
