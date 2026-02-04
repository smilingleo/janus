// Main application component

import { useState } from 'react';
import { LoginPage } from './components/LoginPage';
import { TerminalPage } from './components/TerminalPage';
import './App.css';

function App() {
  const [isAuthenticated, setIsAuthenticated] = useState(false);

  const handleLoginSuccess = () => {
    setIsAuthenticated(true);
  };

  return (
    <div className="app">
      {isAuthenticated ? (
        <TerminalPage />
      ) : (
        <LoginPage onLoginSuccess={handleLoginSuccess} />
      )}
    </div>
  );
}

export default App;
