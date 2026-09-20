import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { invoke } from '@tauri-apps/api/core';

type Field = HTMLInputElement | HTMLTextAreaElement;
type Selection = { x: number; y: number; text: string; field: Field | null; start: number; end: number; target: HTMLElement; range: Range | null };

export function TextContextMenu() {
  const [selection, setSelection] = useState<Selection | null>(null);
  const [error, setError] = useState('');
  const menu = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const open = (event: MouseEvent) => {
      const target = event.target instanceof HTMLElement ? event.target : null;
      if (!target || target.closest('[role="menu"]')) return;
      event.preventDefault();
      const field = target instanceof HTMLTextAreaElement || (target instanceof HTMLInputElement && target.selectionStart !== null) ? target : null;
      const current = window.getSelection();
      const text = field ? (field.type === 'password' ? '' : field.value.slice(field.selectionStart ?? 0, field.selectionEnd ?? 0)) : current?.toString() || target.closest('code, pre')?.textContent || '';
      if (!field && !text) { setSelection(null); return; }
      const bounds = target.getBoundingClientRect();
      setError('');
      setSelection({ x: event.clientX || bounds.left, y: event.clientY || bounds.bottom, text, field, start: field?.selectionStart ?? 0, end: field?.selectionEnd ?? 0, target, range: current?.rangeCount ? current.getRangeAt(0).cloneRange() : null });
    };
    const close = (event: Event) => { if (!menu.current?.contains(event.target as Node)) setSelection(null); };
    const dismiss = () => setSelection(null);
    document.addEventListener('contextmenu', open);
    document.addEventListener('pointerdown', close);
    window.addEventListener('resize', dismiss);
    document.addEventListener('scroll', dismiss, true);
    return () => { document.removeEventListener('contextmenu', open); document.removeEventListener('pointerdown', close); window.removeEventListener('resize', dismiss); document.removeEventListener('scroll', dismiss, true); };
  }, []);
  useLayoutEffect(() => {
    if (!selection || !menu.current) return;
    const node = menu.current;
    const bounds = node.getBoundingClientRect();
    node.style.left = `${Math.max(8, Math.min(selection.x, window.innerWidth - bounds.width - 8))}px`;
    node.style.top = `${Math.max(8, Math.min(selection.y, window.innerHeight - bounds.height - 8))}px`;
    node.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus({ preventScroll: true });
  }, [selection]);
  if (!selection) return error ? <div className="clipboard-error" role="alert">{error}</div> : null;
  const restore = () => {
    selection.target.focus({ preventScroll: true });
    if (selection.field) selection.field.setSelectionRange(selection.start, selection.end);
    else if (selection.range) { const current = window.getSelection(); current?.removeAllRanges(); current?.addRange(selection.range); }
  };
  const replace = (text: string) => {
    const field = selection.field;
    if (!field || field.readOnly || field.disabled) return;
    // Use the native setter so React observes and persists the edit.
    // The DOM setter is deliberately called with the target field as `this`.
    // eslint-disable-next-line @typescript-eslint/unbound-method
    const setter = Object.getOwnPropertyDescriptor(field instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype, 'value')?.set;
    setter?.call(field, field.value.slice(0, selection.start) + text + field.value.slice(selection.end));
    field.dispatchEvent(new Event('input', { bubbles: true }));
    field.setSelectionRange(selection.start + text.length, selection.start + text.length);
  };
  const act = async (action: 'copy' | 'cut' | 'paste' | 'select') => {
    try {
      restore();
      if (action === 'copy' || action === 'cut') await invoke('copy_text', { text: selection.text });
      if (action === 'cut') replace('');
      if (action === 'paste') replace(await invoke<string>('paste_text'));
      if (action === 'select') selection.field?.select();
    } catch (cause) { setError(`Clipboard action failed: ${String(cause)}. You can also use your keyboard shortcuts.`); }
    finally { setSelection(null); }
  };
  const editable = selection.field && !selection.field.readOnly && !selection.field.disabled;
  return createPortal(<div className="text-context-menu" role="menu" aria-label="Text actions" ref={menu} style={{ left: selection.x, top: selection.y }} onKeyDown={(event) => {
    event.stopPropagation();
    if (event.key === 'Escape' || event.key === 'Tab') { event.preventDefault(); restore(); setSelection(null); }
    const buttons = Array.from(menu.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? []);
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      buttons[event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1 : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length]?.focus();
    }
  }}>
    <button role="menuitem" type="button" disabled={!selection.text} onClick={() => void act('copy')}>Copy</button>
    {selection.field ? <>
      <button role="menuitem" type="button" disabled={!editable || !selection.text} onClick={() => void act('cut')}>Cut</button>
      <button role="menuitem" type="button" disabled={!editable} onClick={() => void act('paste')}>Paste</button>
      <button role="menuitem" type="button" onClick={() => void act('select')}>Select all</button>
    </> : null}
  </div>, document.body);
}
