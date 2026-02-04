// Login page component

import { useState } from 'react';
import { apiClient } from '../api';
import './LoginPage.css';

interface LoginPageProps {
  onLoginSuccess: () => void;
}

export function LoginPage({ onLoginSuccess }: LoginPageProps) {
  const [step, setStep] = useState<'request' | 'verify'>('request');
  const [token, setToken] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const handleRequestToken = async () => {
    setLoading(true);
    setError(null);
    setMessage(null);

    try {
      const response = await apiClient.generateToken();
      if (response.success) {
        setMessage(response.message);
        setStep('verify');
      } else {
        setError(response.message || 'Failed to generate token');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to generate token');
    } finally {
      setLoading(false);
    }
  };

  const handleLogin = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError(null);

    try {
      const response = await apiClient.login(token);
      if (response.success) {
        // CSRF token is automatically stored by apiClient
        onLoginSuccess();
      } else {
        setError(response.message || 'Login failed');
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Login failed');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="login-page">
      <div className="login-card">
        <h1>Janus</h1>
        <p className="subtitle">Gateway guardian to your terminal realm</p>

        {step === 'request' ? (
          <div className="login-step">
            <p className="instruction">
              Click the button below to receive an authentication token via iMessage.
            </p>
            <button
              onClick={handleRequestToken}
              disabled={loading}
              className="primary-button"
            >
              {loading ? 'Sending...' : 'Request Token'}
            </button>
            {message && <div className="message success">{message}</div>}
            {error && <div className="message error">{error}</div>}
          </div>
        ) : (
          <div className="login-step">
            <p className="instruction">
              Enter the token you received via iMessage:
            </p>
            <form onSubmit={handleLogin}>
              <input
                type="text"
                value={token}
                onChange={(e) => setToken(e.target.value)}
                placeholder="Enter token"
                className="token-input"
                autoFocus
                disabled={loading}
              />
              <div className="button-group">
                <button
                  type="button"
                  onClick={() => {
                    setStep('request');
                    setToken('');
                    setError(null);
                  }}
                  disabled={loading}
                  className="secondary-button"
                >
                  Back
                </button>
                <button
                  type="submit"
                  disabled={loading || !token.trim()}
                  className="primary-button"
                >
                  {loading ? 'Logging in...' : 'Login'}
                </button>
              </div>
            </form>
            {error && <div className="message error">{error}</div>}
          </div>
        )}
      </div>
    </div>
  );
}
