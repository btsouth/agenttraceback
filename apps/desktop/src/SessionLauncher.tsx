import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Copy, FolderOpen, TerminalSquare } from 'lucide-react';

export function SessionLauncher() {
  const [agent, setAgent] = useState('codex');
  const [project, setProject] = useState('');
  const [transcript, setTranscript] = useState(true);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState('');
  const [error, setError] = useState('');
  const programs: Record<string, string> = { codex: 'codex', 'claude-code': 'claude', 'gemini-cli': 'gemini', opencode: 'opencode', hermes: 'hermes' };
  const launch = async () => {
    setBusy(true); setError(''); setMessage('');
    try {
      const result = await invoke<{ terminal: string }>('launch_recorded_session', { project, agent, transcript });
      setMessage(`Launch sent to ${result.terminal}. Continue in your terminal, including any agent sign-in. The session will appear in Live when recording starts.`);
    } catch (cause) { setError(String(cause)); }
    finally { setBusy(false); }
  };
  return <section className="session-launcher" aria-label="Record a new session">
    <h2><TerminalSquare size={20} /> Record a new session</h2>
    <p>Choose an installed agent and a project. We open your normal terminal with recording enabled—no command to copy.</p>
    <div className="launch-fields">
      <label htmlFor="record-agent">Agent<select id="record-agent" aria-label="Agent" value={agent} disabled={busy} onChange={(event) => { setAgent(event.target.value); setMessage(''); }}>
        <option value="codex">Codex</option><option value="claude-code">Claude Code</option><option value="gemini-cli">Gemini CLI</option><option value="opencode">OpenCode</option><option value="hermes">Hermes</option>
      </select></label>
      <label>Project folder<div className="folder-field"><input value={project} disabled={busy} placeholder="Choose a project folder" onChange={(event) => { setProject(event.target.value); setMessage(''); }} />
        <button className="ghost-button" type="button" disabled={busy} onClick={() => {
          setBusy(true); setError('');
          void invoke<string | null>('pick_project_folder').then((folder) => { if (folder) { setProject(folder); setMessage(''); } }).catch((cause: unknown) => setError(String(cause))).finally(() => setBusy(false));
        }}><FolderOpen size={16} /> Browse</button></div></label>
    </div>
    <label className="transcript-choice"><input type="checkbox" checked={transcript} disabled={busy} onChange={(event) => setTranscript(event.target.checked)} /> Save an encrypted terminal transcript</label>
    <p className="command-caption">The agent runs with your normal account and project permissions. Stop it in the terminal when finished.</p>
    <button className="primary-button" type="button" disabled={busy || !project.trim()} onClick={() => void launch()}><TerminalSquare size={16} /> {busy ? 'Opening…' : 'Start recording in terminal'}</button>
    {message ? <p role="status" className="success-copy">{message}</p> : null}
    {error ? <p role="alert" className="launch-error">{error}</p> : null}
    <details className="manual-recording"><summary>Already using the CLI?</summary><p>If agenttraceback is installed on your PATH, run this from your project folder:</p><CopyCommand text={`agenttraceback run --agent ${agent}${transcript ? '' : ' --no-transcript'} -- ${programs[agent]}`} /></details>
  </section>;
}

export function CopyCommand({ text }: { text: string }) {
  const [status, setStatus] = useState('');
  return <div className="copy-command"><code>{text}</code><button className="ghost-button" type="button" onClick={() => {
    void invoke('copy_text', { text }).then(() => setStatus('Copied')).catch(() => setStatus('Copy failed. Select the text and press Ctrl+C or ⌘C.'));
  }}><Copy size={15} /> Copy</button><span role="status">{status}</span></div>;
}
