// API types matching backend response structures

export interface HealthResponse {
  status: string;
  version: string;
  uptime_secs: number;
}

export interface TokenGenerateResponse {
  success: boolean;
  message: string;
}

export interface LoginRequest {
  token: string;
}

export interface LoginResponse {
  success: boolean;
  message: string;
  csrf_token?: string;
  session_duration_secs?: number;
}

export interface CreateSessionRequest {
  shell_command?: string;
  rows?: number;
  cols?: number;
}

export interface CreateSessionResponse {
  success: boolean;
  message: string;
  session_id?: string;
}

export interface SessionInfo {
  id: string;
  created_at: number; // SystemTime as epoch seconds
  last_activity_secs_ago: number;
  pty_rows: number;
  pty_cols: number;
}

export interface ListSessionsResponse {
  success: boolean;
  sessions: SessionInfo[];
}

export interface DeleteSessionResponse {
  success: boolean;
  message: string;
}

// WebSocket message types
export type TerminalMessage =
  | { type: 'output'; data: number[] }
  | { type: 'input'; data: number[] }
  | { type: 'resize'; rows: number; cols: number }
  | { type: 'ping' }
  | { type: 'pong' }
  | { type: 'error'; message: string }
  | { type: 'attached'; session_id: string };

export class ApiError extends Error {
  status?: number;
  response?: any;

  constructor(
    message: string,
    status?: number,
    response?: any
  ) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
    this.response = response;
  }
}
