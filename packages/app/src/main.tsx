import { StrictMode } from 'react';
import ReactDOM from 'react-dom/client';
import MyComputerApp from './MyComputerApp';
import './styles/index.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <MyComputerApp />
  </StrictMode>,
);
