// API client for backend communication with CSRF support

import type {
  HealthResponse,
  TokenGenerateResponse,
  LoginRequest,
  LoginResponse,
  CreateSessionRequest,
  CreateSessionResponse,
  ListSessionsResponse,
  DeleteSessionResponse,
} from './types';
import { ApiError } from './types';

// Type for header record
type HeaderRecord = Record<string, string>;

class ApiClient {
  private baseUrl: string;
  private csrfToken: string | null = null;

  constructor(baseUrl: string = '') {
    // Default to same origin for production, can be overridden for development
    this.baseUrl = baseUrl || window.location.origin;
  }

  /**
   * Set CSRF token (obtained from login response)
   */
  setCSRFToken(token: string) {
    this.csrfToken = token;
  }

  /**
   * Get current CSRF token
   */
  getCSRFToken(): string | null {
    return this.csrfToken;
  }

  /**
   * Clear CSRF token (on logout)
   */
  clearCSRFToken() {
    this.csrfToken = null;
  }

  /**
   * Make HTTP request with CSRF token and credentials
   */
  private async request<T>(
    path: string,
    options: RequestInit = {}
  ): Promise<T> {
    const url = `${this.baseUrl}${path}`;

    const headers: HeaderRecord = {
      'Content-Type': 'application/json',
    };

    // Add CSRF token if available (for state-changing operations)
    if (this.csrfToken && options.method && options.method !== 'GET') {
      headers['X-CSRF-Token'] = this.csrfToken;
    }

    const response = await fetch(url, {
      ...options,
      headers: {
        ...headers,
        ...(options.headers as HeaderRecord),
      },
      credentials: 'include', // Include cookies (session_id)
    });

    if (!response.ok) {
      const errorText = await response.text();
      let errorData;
      try {
        errorData = JSON.parse(errorText);
      } catch {
        errorData = { message: errorText };
      }

      throw new ApiError(
        errorData.message || `HTTP ${response.status}: ${response.statusText}`,
        response.status,
        errorData
      );
    }

    return response.json();
  }

  // ==================== Health Check ====================

  async health(): Promise<HealthResponse> {
    return this.request<HealthResponse>('/api/health');
  }

  // ==================== Authentication ====================

  async generateToken(): Promise<TokenGenerateResponse> {
    return this.request<TokenGenerateResponse>('/api/token/generate', {
      method: 'POST',
    });
  }

  async login(token: string): Promise<LoginResponse> {
    const request: LoginRequest = { token };
    const response = await this.request<LoginResponse>('/api/auth/login', {
      method: 'POST',
      body: JSON.stringify(request),
    });

    // Store CSRF token from login response
    if (response.success && response.csrf_token) {
      this.setCSRFToken(response.csrf_token);
    }

    return response;
  }

  // ==================== Session Management ====================

  async listSessions(): Promise<ListSessionsResponse> {
    return this.request<ListSessionsResponse>('/api/sessions');
  }

  async createSession(
    options: CreateSessionRequest = {}
  ): Promise<CreateSessionResponse> {
    return this.request<CreateSessionResponse>('/api/sessions', {
      method: 'POST',
      body: JSON.stringify(options),
    });
  }

  async deleteSession(sessionId: string): Promise<DeleteSessionResponse> {
    return this.request<DeleteSessionResponse>(`/api/sessions/${sessionId}`, {
      method: 'DELETE',
    });
  }

  // ==================== WebSocket ====================

  /**
   * Get WebSocket URL for a session
   */
  getWebSocketUrl(sessionId: string): string {
    const protocol = this.baseUrl.startsWith('https') ? 'wss' : 'ws';
    const host = this.baseUrl.replace(/^https?:\/\//, '');
    return `${protocol}://${host}/api/sessions/${sessionId}/ws`;
  }
}

// Export singleton instance
export const apiClient = new ApiClient();

// Export class for testing
export { ApiClient };
