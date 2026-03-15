import { useState, useEffect } from 'react';
import { ThemeProvider, createTheme, CssBaseline } from '@mui/material';
import {
  AppBar, Toolbar, Typography, Container, Paper, Box, Button, TextField,
  Select, MenuItem, FormControl, InputLabel, CircularProgress, Stack
} from '@mui/material';
import { ComponentRenderer } from './adk-ui-renderer/Renderer';
import { convertA2UIComponent } from './adk-ui-renderer/a2ui-converter';
import type { Component } from './adk-ui-renderer/types';

const theme = createTheme({
  palette: {
    mode: 'light',
    primary: { main: '#1976d2' },
    secondary: { main: '#9c27b0' },
  },
  typography: {
    fontFamily: '"Roboto", "Helvetica", "Arial", sans-serif',
  },
  shape: { borderRadius: 8 },
});

interface Surface {
  surfaceId: string;
  components: Component[];
  dataModel: Record<string, unknown>;
}

interface Example {
  id: string;
  name: string;
  port: number;
  prompts: string[];
}

const EXAMPLES: Example[] = [
  { id: 'ui_demo', name: 'UI Demo', port: 8080, prompts: ['Show me a welcome screen', 'Create a user profile card', 'Build a settings form'] },
  { id: 'ui_working_support', name: 'Support', port: 8081, prompts: ['Open a support ticket', 'Report a bug', 'App crashing issue'] },
  { id: 'ui_working_appointment', name: 'Appointments', port: 8082, prompts: ['Book appointment', 'Show services', 'Schedule follow-up'] },
  { id: 'ui_working_events', name: 'Events', port: 8083, prompts: ['RSVP for launch', 'Show agenda', 'Register 2 guests'] },
  { id: 'ui_working_facilities', name: 'Facilities', port: 8084, prompts: ['Report leak', 'Work order', 'HVAC repair'] },
  { id: 'ui_working_inventory', name: 'Inventory', port: 8085, prompts: ['Restock SKU', 'Low stock items', 'Purchase request'] },
];

function App() {
  const [surface, setSurface] = useState<Surface | null>(null);
  const [selectedExample, setSelectedExample] = useState<Example>(EXAMPLES[0]);
  const [isLoading, setIsLoading] = useState(false);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [customPrompt, setCustomPrompt] = useState('');

  const sendMessage = async (message: string) => {
    if (!message.trim()) return;
    setIsLoading(true);
    setSurface(null);

    try {
      const baseUrl = `http://localhost:${selectedExample.port}`;
      let sid = sessionId;
      
      if (!sid) {
        const res = await fetch(`${baseUrl}/api/apps/${selectedExample.id}/users/user1/sessions`, {
          method: 'POST', headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ state: {} }),
        });
        if (!res.ok) return;
        sid = (await res.json()).id;
        setSessionId(sid);
      }

      const response = await fetch(`${baseUrl}/api/run/${selectedExample.id}/user1/${sid}`, {
        method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ new_message: message }),
      });

      if (!response.ok || !response.body) return;

      const reader = response.body.getReader();
      const decoder = new TextDecoder();

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        for (const line of decoder.decode(value).split('\n')) {
          if (!line.startsWith('data: ')) continue;
          const data = line.slice(6).trim();
          if (!data) continue;

          try {
            const evt = JSON.parse(data);
            if (evt.content?.parts) {
              for (const part of evt.content.parts) {
                if (part.functionResponse?.name === 'render_screen') {
                  const res = part.functionResponse.response;
                  if (res.components) {
                    const arr = typeof res.components === 'string' ? JSON.parse(res.components) : res.components;
                    const map = new Map<string, any>();
                    arr.forEach((c: any) => {
                      const conv = convertA2UIComponent(c);
                      if (conv) map.set(conv.id, conv);
                    });
                    const resolve = (c: any): any => {
                      // Resolve button child text
                      if (c.type === 'button' && c.child_id) {
                        const childText = map.get(c.child_id);
                        if (childText?.content) {
                          c.label = childText.content;
                        }
                      }
                      // Resolve children arrays
                      if (c.children?.length) {
                        return { ...c, children: c.children.map((id: string) => map.get(id) ? resolve(map.get(id)) : null).filter(Boolean) };
                      }
                      return c;
                    };
                    const root = map.get('root');
                    if (root) setSurface({ surfaceId: res.surface_id || 'main', components: [resolve(root)], dataModel: res.data_model || {} });
                  }
                }
              }
            }
          } catch {}
        }
      }
    } finally {
      setIsLoading(false);
    }
  };

  useEffect(() => { setSessionId(null); setSurface(null); }, [selectedExample]);

  return (
    <ThemeProvider theme={theme}>
      <CssBaseline />
      <Box sx={{ minHeight: '100vh', bgcolor: 'grey.100' }}>
        <AppBar position="static" elevation={1}>
          <Toolbar>
            <Typography variant="h6" sx={{ flexGrow: 1 }}>A2UI Material Design</Typography>
            <FormControl size="small" sx={{ minWidth: 150 }}>
              <Select
                value={selectedExample.id}
                onChange={(e) => setSelectedExample(EXAMPLES.find(ex => ex.id === e.target.value) || EXAMPLES[0])}
                sx={{ bgcolor: 'white', borderRadius: 1 }}
              >
                {EXAMPLES.map(ex => <MenuItem key={ex.id} value={ex.id}>{ex.name}</MenuItem>)}
              </Select>
            </FormControl>
          </Toolbar>
        </AppBar>

        <Container maxWidth="lg" sx={{ py: 4 }}>
          <Stack direction={{ xs: 'column', md: 'row' }} spacing={3}>
            {/* Prompts */}
            <Paper sx={{ p: 3, width: { xs: '100%', md: 300 }, flexShrink: 0 }}>
              <Typography variant="h6" gutterBottom>Quick Prompts</Typography>
              <Stack spacing={1}>
                {selectedExample.prompts.map((p, i) => (
                  <Button key={i} variant="outlined" fullWidth onClick={() => sendMessage(p)} disabled={isLoading}
                    sx={{ justifyContent: 'flex-start', textTransform: 'none' }}>
                    {p}
                  </Button>
                ))}
              </Stack>
              <Box sx={{ mt: 3 }}>
                <TextField
                  fullWidth size="small" label="Custom prompt" value={customPrompt}
                  onChange={(e) => setCustomPrompt(e.target.value)}
                  onKeyDown={(e) => e.key === 'Enter' && (sendMessage(customPrompt), setCustomPrompt(''))}
                  disabled={isLoading}
                />
                <Button fullWidth variant="contained" sx={{ mt: 1 }} disabled={isLoading || !customPrompt}
                  onClick={() => { sendMessage(customPrompt); setCustomPrompt(''); }}>
                  Send
                </Button>
              </Box>
            </Paper>

            {/* UI Display */}
            <Paper sx={{ p: 3, flexGrow: 1, minHeight: 400 }}>
              {surface?.components.length ? (
                surface.components.map((c, i) => <ComponentRenderer key={i} component={c} />)
              ) : (
                <Box sx={{ display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', height: 300 }}>
                  {isLoading ? (
                    <><CircularProgress sx={{ mb: 2 }} /><Typography color="text.secondary">Generating UI...</Typography></>
                  ) : (
                    <Typography color="text.secondary">Select a prompt to generate UI</Typography>
                  )}
                </Box>
              )}
            </Paper>
          </Stack>
        </Container>
      </Box>
    </ThemeProvider>
  );
}

export default App;
