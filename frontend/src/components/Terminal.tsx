// Terminal component with xterm.js

import { useEffect, useRef, useState } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { apiClient } from '../api';
import type { TerminalMessage } from '../api';
import '@xterm/xterm/css/xterm.css';
import './Terminal.css';

interface TerminalProps {
  sessionId: string;
  onClose?: () => void;
  onSessionEnded?: () => void; // Called when shell exits (no confirmation needed)
  onError?: (error: Error) => void;
}

export function Terminal({ sessionId, onClose, onSessionEnded, onError }: TerminalProps) {
  const terminalRef = useRef<HTMLDivElement>(null);
  const xtermRef = useRef<XTerm | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const [status, setStatus] = useState<'connecting' | 'connected' | 'disconnected'>('connecting');

  useEffect(() => {
    if (!terminalRef.current) return;

    // Initialize xterm.js
    const xterm = new XTerm({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: 'Menlo, Monaco, "Courier New", monospace',
      theme: {
        background: '#1e1e1e',
        foreground: '#d4d4d4',
        cursor: '#d4d4d4',
        black: '#000000',
        red: '#cd3131',
        green: '#0dbc79',
        yellow: '#e5e510',
        blue: '#2472c8',
        magenta: '#bc3fbc',
        cyan: '#11a8cd',
        white: '#e5e5e5',
        brightBlack: '#666666',
        brightRed: '#f14c4c',
        brightGreen: '#23d18b',
        brightYellow: '#f5f543',
        brightBlue: '#3b8eea',
        brightMagenta: '#d670d6',
        brightCyan: '#29b8db',
        brightWhite: '#ffffff',
      },
    });

    // Add addons
    const fitAddon = new FitAddon();
    xterm.loadAddon(fitAddon);
    xterm.loadAddon(new WebLinksAddon());

    // Open terminal in DOM
    xterm.open(terminalRef.current);
    fitAddon.fit();

    xtermRef.current = xterm;
    fitAddonRef.current = fitAddon;

    // Connect WebSocket
    const wsUrl = apiClient.getWebSocketUrl(sessionId);
    const ws = new WebSocket(wsUrl);
    wsRef.current = ws;

    ws.onopen = () => {
      setStatus('connected');
      xterm.writeln('\x1b[32mConnected to terminal session\x1b[0m');
    };

    ws.onmessage = (event) => {
      try {
        const message: TerminalMessage = JSON.parse(event.data);

        switch (message.type) {
          case 'output':
            // Write output to terminal
            const data = new Uint8Array(message.data);
            xterm.write(data);
            break;

          case 'attached':
            xterm.writeln(`\x1b[32mAttached to session: ${message.session_id}\x1b[0m`);
            break;

          case 'error':
            // Check if this is a session ended message (shell exited)
            if (message.message === 'Session ended') {
              xterm.writeln('\x1b[33mShell exited. Session will be closed.\x1b[0m');
              setStatus('disconnected');
              // Close WebSocket and trigger automatic cleanup (no confirmation)
              setTimeout(() => {
                ws.close();
                if (onSessionEnded) {
                  onSessionEnded();
                }
              }, 1000);
            } else {
              xterm.writeln(`\x1b[31mError: ${message.message}\x1b[0m`);
              if (onError) {
                onError(new Error(message.message));
              }
            }
            break;

          case 'pong':
            // Keepalive response, no action needed
            break;

          default:
            console.log('Unknown message type:', message);
        }
      } catch (err) {
        console.error('Failed to parse WebSocket message:', err);
      }
    };

    ws.onerror = (error) => {
      console.error('WebSocket error:', error);
      setStatus('disconnected');
      xterm.writeln('\x1b[31mWebSocket error occurred\x1b[0m');
      if (onError) {
        onError(new Error('WebSocket error'));
      }
    };

    ws.onclose = () => {
      setStatus('disconnected');
      xterm.writeln('\x1b[33mConnection closed\x1b[0m');
    };

    // Custom key handler for Shift+Enter multi-line support
    xterm.attachCustomKeyEventHandler((event) => {
      // Intercept Shift+Enter to send literal newline
      if (event.key === 'Enter' && event.shiftKey && event.type === 'keydown') {
        if (ws.readyState === WebSocket.OPEN) {
          // Send literal newline character
          const message: TerminalMessage = {
            type: 'input',
            data: [10], // \n character
          };
          ws.send(JSON.stringify(message));
        }
        return false; // Prevent default handling
      }
      return true; // Let xterm.js handle other keys normally
    });

    // Track input buffer to detect 'exit' command
    let inputBuffer = '';

    // Handle terminal input (normal characters and Enter)
    xterm.onData((data) => {
      if (ws.readyState === WebSocket.OPEN) {
        const bytes = Array.from(new TextEncoder().encode(data));

        // Check if this is Enter key (carriage return)
        if (data === '\r' || data === '\n') {
          // Check if the buffered input is 'exit' command
          const trimmedBuffer = inputBuffer.trim();
          if (trimmedBuffer === 'exit') {
            // Show confirmation dialog
            if (confirm('Are you sure you want to exit this terminal session?')) {
              // User confirmed, send the Enter to execute exit command
              const message: TerminalMessage = {
                type: 'input',
                data: bytes,
              };
              ws.send(JSON.stringify(message));
            } else {
              // User cancelled, send Ctrl+C to cancel the command
              const ctrlC: TerminalMessage = {
                type: 'input',
                data: [3], // Ctrl+C
              };
              ws.send(JSON.stringify(ctrlC));
            }
            inputBuffer = '';
            return;
          }
          // Not an exit command, send normally and reset buffer
          inputBuffer = '';
        } else if (data === '\x7f' || data === '\b') {
          // Backspace - remove last character from buffer
          inputBuffer = inputBuffer.slice(0, -1);
        } else if (data === '\x03') {
          // Ctrl+C - clear buffer
          inputBuffer = '';
        } else if (data.charCodeAt(0) >= 32 && data.charCodeAt(0) < 127) {
          // Printable ASCII character - add to buffer
          inputBuffer += data;
        }

        // Send the data to the server
        const message: TerminalMessage = {
          type: 'input',
          data: bytes,
        };
        ws.send(JSON.stringify(message));
      }
    });

    // Handle resize
    const handleResize = () => {
      if (fitAddon && ws.readyState === WebSocket.OPEN) {
        fitAddon.fit();
        const message: TerminalMessage = {
          type: 'resize',
          rows: xterm.rows,
          cols: xterm.cols,
        };
        ws.send(JSON.stringify(message));
      }
    };

    // Set up resize observer
    const resizeObserver = new ResizeObserver(() => {
      handleResize();
    });

    if (terminalRef.current) {
      resizeObserver.observe(terminalRef.current);
    }

    // Initial resize
    setTimeout(() => handleResize(), 100);

    // Cleanup
    return () => {
      resizeObserver.disconnect();
      ws.close();
      xterm.dispose();
    };
  }, [sessionId, onError]);

  return (
    <div className="terminal-container">
      <div className="terminal-header">
        <div className="terminal-title">Session: {sessionId.split('-').slice(-1)[0]}</div>
        <div className="terminal-status">
          <span className={`status-indicator ${status}`}></span>
          {status === 'connected' && 'Connected'}
          {status === 'connecting' && 'Connecting...'}
          {status === 'disconnected' && 'Disconnected'}
        </div>
        {onClose && (
          <button className="terminal-close" onClick={onClose} aria-label="Close terminal">
            ×
          </button>
        )}
      </div>
      <div className="terminal-wrapper" ref={terminalRef}></div>
    </div>
  );
}
