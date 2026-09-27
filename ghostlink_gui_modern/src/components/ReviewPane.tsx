import { resolveApiBase } from '../config';
import React, { useState } from 'react';
import { DiffEditor } from '@monaco-editor/react';
import { Check, X, RefreshCw, AlertTriangle, Terminal, Shield, FileText } from 'lucide-react';
import { ReviewPacket, ReviewDiff, GhostlinkAPI } from '../api';
import { useAppStore } from '../store';

function guessLanguage(path: string): string {
  const ext = path.split('.').pop()?.toLowerCase();
  switch (ext) {
    case 'rs':
      return 'rust';
    case 'ts':
    case 'tsx':
      return 'typescript';
    case 'js':
    case 'jsx':
      return 'javascript';
    case 'py':
      return 'python';
    case 'json':
      return 'json';
    case 'html':
      return 'html';
    case 'css':
      return 'css';
    case 'md':
      return 'markdown';
    case 'go':
      return 'go';
    case 'sh':
      return 'shell';
    case 'toml':
      return 'toml';
    case 'yaml':
    case 'yml':
      return 'yaml';
    default:
      return 'plaintext';
  }
}

interface ReviewPaneProps {
  packet: ReviewPacket;
  api?: GhostlinkAPI;
  onDecided?: () => void;
}

export const ReviewPane: React.FC<ReviewPaneProps> = ({ packet, api: propApi, onDecided }) => {
  const api = propApi || new GhostlinkAPI(resolveApiBase());

  const [selectedDiffIndex, setSelectedDiffIndex] = useState<number>(0);
  const [deciding, setDeciding] = useState<boolean>(false);
  const [note, setNote] = useState<string>('');
  const [showNoteInput, setShowNoteNoteInput] = useState<boolean>(false);

  const addToast = useAppStore((state) => state.addToast);
  const currentDiff: ReviewDiff | undefined = packet.diffs[selectedDiffIndex];

  const handleDecide = async (decision: 'accept' | 'request_changes' | 'reject') => {
    setDeciding(true);
    try {
      await api.decideReview(packet.id, { decision, note: note.trim() || undefined });
      addToast({
        message: `Review decision '${decision}' applied successfully.`,
        type: 'success',
      });
      if (onDecided) onDecided();
    } catch (err: any) {
      addToast({
        message: err.response?.data?.error || err.message || 'Failed to submit decision',
        type: 'error',
      });
    } finally {
      setDeciding(false);
    }
  };

  return (
    <div className="flex flex-col h-full bg-slate-900 border border-slate-800 rounded-xl overflow-hidden shadow-2xl">
      {/* Header */}
      <div className="px-6 py-4 bg-slate-950 border-b border-slate-800 flex flex-wrap items-center justify-between gap-4">
        <div>
          <div className="flex items-center gap-2">
            <Shield className="w-5 h-5 text-indigo-400" aria-hidden="true" />
            <h2 className="text-lg font-bold text-slate-100">ReviewPacket #{packet.id.slice(0, 8)}</h2>
          </div>
          <p className="text-sm text-slate-400 mt-1">{packet.summary}</p>
        </div>

        {/* Action Buttons */}
        <div className="flex items-center gap-2">
          {showNoteInput && (
            <input
              type="text"
              placeholder="Note for request changes..."
              value={note}
              onChange={(e) => setNote(e.target.value)}
              className="px-3 py-1.5 bg-slate-900 border border-slate-700 rounded-lg text-xs text-slate-200 focus:outline-none focus:ring-2 focus:ring-amber-500"
            />
          )}
          <button
            onClick={() => handleDecide('accept')}
            disabled={deciding}
            aria-label="Accept proposed changes"
            title="Accept proposed changes and apply to project root"
            className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-xs font-bold bg-green-600 hover:bg-green-500 text-white transition disabled:opacity-50"
          >
            <Check size={14} aria-hidden="true" /> Accept
          </button>
          <button
            onClick={() => {
              if (!showNoteInput) {
                setShowNoteNoteInput(true);
              } else {
                handleDecide('request_changes');
              }
            }}
            disabled={deciding}
            aria-label="Request changes"
            title="Request changes and requeue task with note"
            className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-xs font-bold bg-amber-600 hover:bg-amber-500 text-white transition disabled:opacity-50"
          >
            <RefreshCw size={14} aria-hidden="true" /> Request Changes
          </button>
          <button
            onClick={() => handleDecide('reject')}
            disabled={deciding}
            aria-label="Reject proposed changes"
            title="Reject proposed changes and delete staging tree"
            className="flex items-center gap-1.5 px-4 py-2 rounded-lg text-xs font-bold bg-rose-700 hover:bg-rose-600 text-white transition disabled:opacity-50"
          >
            <X size={14} aria-hidden="true" /> Reject
          </button>
        </div>
      </div>

      {/* Main Content split: Left diffs list/details, Right diff view */}
      <div className="flex-1 flex min-h-0">
        {/* Left Sidebar: Diffs list, commands, risks */}
        <div className="w-80 border-r border-slate-800 bg-slate-950 flex flex-col overflow-y-auto">
          {/* File Diffs List */}
          <div className="p-4 border-b border-slate-800">
            <h3 className="text-xs font-semibold uppercase text-slate-400 mb-2 flex items-center gap-1.5">
              <FileText size={14} aria-hidden="true" /> Proposed Diffs ({packet.diffs.length})
            </h3>
            {packet.diffs.length === 0 ? (
              <p className="text-xs text-slate-500 italic">No file changes proposed</p>
            ) : (
              <div className="space-y-1">
                {packet.diffs.map((diff, idx) => (
                  <button
                    key={diff.path}
                    onClick={() => setSelectedDiffIndex(idx)}
                    className={`w-full text-left px-3 py-2 rounded-lg text-xs font-mono transition flex items-center justify-between ${
                      selectedDiffIndex === idx
                        ? 'bg-indigo-600/20 text-indigo-300 border border-indigo-500/30'
                        : 'text-slate-300 hover:bg-slate-900'
                    }`}
                  >
                    <span className="truncate">{diff.path}</span>
                  </button>
                ))}
              </div>
            )}
          </div>

          {/* Commands Run */}
          {packet.commands && packet.commands.length > 0 && (
            <div className="p-4 border-b border-slate-800">
              <h3 className="text-xs font-semibold uppercase text-slate-400 mb-2 flex items-center gap-1.5">
                <Terminal size={14} aria-hidden="true" /> Commands Evaluated ({packet.commands.length})
              </h3>
              <div className="space-y-2">
                {packet.commands.map((cmd, idx) => (
                  <div key={idx} className="p-2 bg-slate-900 rounded border border-slate-800 text-xs">
                    <div className="flex items-center justify-between">
                      <span className="font-mono text-slate-200 truncate">{cmd.argv.join(' ')}</span>
                      <span
                        className={`text-[10px] px-1.5 py-0.5 rounded font-bold uppercase ${
                          cmd.judge === 'allow'
                            ? 'bg-green-500/20 text-green-400'
                            : cmd.judge === 'deny'
                            ? 'bg-rose-500/20 text-rose-400'
                            : 'bg-amber-500/20 text-amber-400'
                        }`}
                      >
                        {cmd.judge}
                      </span>
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* Risks */}
          {packet.risks && packet.risks.length > 0 && (
            <div className="p-4">
              <h3 className="text-xs font-semibold uppercase text-amber-400 mb-2 flex items-center gap-1.5">
                <AlertTriangle size={14} aria-hidden="true" /> Residual Risks ({packet.risks.length})
              </h3>
              <ul className="list-disc list-inside text-xs text-slate-400 space-y-1">
                {packet.risks.map((risk, idx) => (
                  <li key={idx}>{risk}</li>
                ))}
              </ul>
            </div>
          )}
        </div>

        {/* Right Pane: Monaco DiffEditor */}
        <div className="flex-1 min-h-0 relative bg-slate-900">
          {currentDiff ? (
            <DiffEditor
              original={currentDiff.original}
              modified={currentDiff.proposed}
              language={guessLanguage(currentDiff.path)}
              theme="vs-dark"
              options={{ readOnly: true, renderSideBySide: true, minimap: { enabled: false } }}
            />
          ) : (
            <div className="h-full flex items-center justify-center text-slate-500 text-sm">
              Select a diff file to review
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
