import { useEffect, useState } from 'react';
import { DeskPage } from './components/desk/DeskPage';
import './styles/cockpit.css';
import './styles/my-computer.css';

function preferredTheme(): 'light' | 'dark' {
  if (typeof window === 'undefined' || !window.matchMedia) return 'dark';
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

export default function MyComputerApp() {
  const [theme, setTheme] = useState<'light' | 'dark'>(preferredTheme);

  useEffect(() => {
    if (!window.matchMedia) return;
    const query = window.matchMedia('(prefers-color-scheme: light)');
    const onChange = () => setTheme(query.matches ? 'light' : 'dark');
    query.addEventListener('change', onChange);
    return () => query.removeEventListener('change', onChange);
  }, []);

  return (
    <div className="cockpit ck-solo" data-theme={theme}>
      <main className="ck-main">
        <DeskPage />
      </main>
    </div>
  );
}
