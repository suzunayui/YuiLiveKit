'use strict';

window.__TAURI__.core.invoke('server_state').then(state => {
  document.getElementById('url').textContent = state.url;
  const error = document.getElementById('error');
  error.textContent = state.error || '';
  error.hidden = !state.error;
});
