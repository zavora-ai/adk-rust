import React from 'react';
import {
  Typography,
  Button,
  TextField,
  Select,
  MenuItem,
  FormControl,
  InputLabel,
  Card,
  CardContent,
  CardHeader,
  CardActions,
  Box,
  Stack,
  Grid,
  Divider,
  Alert,
  CircularProgress,
  LinearProgress,
  Chip,
  Switch,
  FormControlLabel,
  Slider,
  Tabs,
  Tab,
  Table,
  TableBody,
  TableCell,
  TableContainer,
  TableHead,
  TableRow,
  Paper,
  Skeleton,
} from '@mui/material';
import * as Icons from '@mui/icons-material';
import type { Component, UiEvent } from './types';

interface RendererProps {
  component: Component;
  onAction?: (event: UiEvent) => void;
}

const getIcon = (name: string, props?: any) => {
  const iconName = name.split(/[-_]/).map(s => s.charAt(0).toUpperCase() + s.slice(1)).join('');
  const IconComponent = (Icons as any)[iconName] || (Icons as any)[iconName + 'Outlined'] || Icons.HelpOutline;
  return <IconComponent {...props} />;
};

export const ComponentRenderer: React.FC<RendererProps> = ({ component, onAction }) => {
  const [tabValue, setTabValue] = React.useState(0);

  switch (component.type) {
    case 'text':
      const variantMap: Record<string, any> = {
        h1: 'h4', h2: 'h5', h3: 'h6', h4: 'subtitle1',
        body: 'body1', caption: 'caption', label: 'subtitle2',
      };
      return (
        <Typography variant={variantMap[component.variant || 'body'] || 'body1'} gutterBottom>
          {component.content}
        </Typography>
      );

    case 'button':
      return (
        <Button
          variant={component.variant === 'secondary' ? 'outlined' : 'contained'}
          color={component.variant === 'danger' ? 'error' : 'primary'}
          disabled={component.disabled}
          startIcon={component.icon ? getIcon(component.icon) : undefined}
          onClick={() => onAction?.({ type: 'action', action_id: component.action_id || component.id })}
          sx={{ textTransform: 'none', mr: 1 }}
        >
          {component.label}
        </Button>
      );

    case 'icon':
      return getIcon(component.name || 'help', { fontSize: component.size || 'medium' });

    case 'alert':
      return (
        <Alert severity={component.variant as any || 'info'} sx={{ mb: 2 }}>
          {component.title && <strong>{component.title}: </strong>}
          {component.message}
        </Alert>
      );

    case 'card':
      return (
        <Card elevation={2} sx={{ mb: 2 }}>
          {component.title && <CardHeader title={component.title} subheader={component.subtitle} />}
          <CardContent>
            {component.content?.map((child, i) => (
              <ComponentRenderer key={i} component={child} onAction={onAction} />
            ))}
          </CardContent>
          {component.footer && (
            <CardActions>
              {component.footer.map((child, i) => (
                <ComponentRenderer key={i} component={child} onAction={onAction} />
              ))}
            </CardActions>
          )}
        </Card>
      );

    case 'stack':
      return (
        <Stack direction={component.direction === 'horizontal' ? 'row' : 'column'} spacing={2} sx={{ mb: 2 }}>
          {component.children?.map((child, i) => (
            <ComponentRenderer key={i} component={child} onAction={onAction} />
          ))}
        </Stack>
      );

    case 'text_input':
      return (
        <TextField
          fullWidth label={component.label} name={component.name}
          type={component.input_type || 'text'} placeholder={component.placeholder}
          required={component.required} defaultValue={component.default_value}
          variant="outlined" size="small" sx={{ mb: 2 }}
        />
      );

    case 'textarea':
      return (
        <TextField
          fullWidth label={component.label} name={component.name}
          placeholder={component.placeholder} required={component.required}
          defaultValue={component.default_value} multiline rows={component.rows || 4}
          variant="outlined" size="small" sx={{ mb: 2 }}
        />
      );

    case 'number_input':
      return (
        <TextField
          fullWidth label={component.label} name={component.name} type="number"
          inputProps={{ min: component.min, max: component.max, step: component.step }}
          required={component.required} defaultValue={component.default_value}
          variant="outlined" size="small" sx={{ mb: 2 }}
        />
      );

    case 'select':
      return (
        <FormControl fullWidth size="small" sx={{ mb: 2 }}>
          <InputLabel>{component.label}</InputLabel>
          <Select label={component.label} name={component.name} defaultValue="">
            {component.options?.map((opt, i) => (
              <MenuItem key={i} value={opt.value}>{opt.label}</MenuItem>
            ))}
          </Select>
        </FormControl>
      );

    case 'switch':
      return (
        <FormControlLabel
          control={<Switch defaultChecked={component.default_checked} name={component.name} />}
          label={component.label} sx={{ mb: 2 }}
        />
      );

    case 'slider':
      return (
        <Box sx={{ mb: 2 }}>
          <Typography gutterBottom>{component.label}</Typography>
          <Slider
            defaultValue={component.default_value || component.min || 0}
            min={component.min} max={component.max} step={component.step}
            valueLabelDisplay="auto"
          />
        </Box>
      );

    case 'progress':
      return (
        <Box sx={{ mb: 2 }}>
          {component.label && <Typography variant="body2" gutterBottom>{component.label}</Typography>}
          <LinearProgress variant="determinate" value={component.value || 0} />
        </Box>
      );

    case 'spinner':
      return (
        <Box sx={{ display: 'flex', justifyContent: 'center', p: 2 }}>
          <CircularProgress size={component.size === 'small' ? 24 : component.size === 'large' ? 48 : 36} />
        </Box>
      );

    case 'skeleton':
      return <Skeleton variant="rectangular" width={component.width || '100%'} height={component.height || 40} sx={{ mb: 1 }} />;

    case 'badge':
      return <Chip label={component.label} color={component.variant as any || 'default'} size="small" sx={{ mr: 1 }} />;

    case 'divider':
      return <Divider sx={{ my: 2 }} />;

    case 'image':
      return <Box component="img" src={component.src} alt={component.alt || ''} sx={{ maxWidth: '100%', borderRadius: 1, mb: 2 }} />;

    case 'grid':
      return (
        <Grid container spacing={2} sx={{ mb: 2 }}>
          {component.children?.map((child, i) => (
            <Grid item xs={12} sm={6} md={12 / (component.columns || 2)} key={i}>
              <ComponentRenderer component={child} onAction={onAction} />
            </Grid>
          ))}
        </Grid>
      );

    case 'table':
      return (
        <TableContainer component={Paper} sx={{ mb: 2 }}>
          <Table size="small">
            <TableHead>
              <TableRow>
                {component.columns?.map((col, i) => (
                  <TableCell key={i} sx={{ fontWeight: 'bold' }}>{col.label}</TableCell>
                ))}
              </TableRow>
            </TableHead>
            <TableBody>
              {component.rows?.map((row, i) => (
                <TableRow key={i} hover>
                  {component.columns?.map((col, j) => (
                    <TableCell key={j}>{row[col.key]}</TableCell>
                  ))}
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </TableContainer>
      );

    case 'tabs':
      return (
        <Box sx={{ mb: 2 }}>
          <Tabs value={tabValue} onChange={(_, v) => setTabValue(v)}>
            {component.tabs?.map((tab, i) => <Tab key={i} label={tab.label} />)}
          </Tabs>
          <Box sx={{ p: 2 }}>
            {component.tabs?.[tabValue]?.content?.map((child, i) => (
              <ComponentRenderer key={i} component={child} onAction={onAction} />
            ))}
          </Box>
        </Box>
      );

    case 'key_value':
      return (
        <Box sx={{ mb: 2 }}>
          {component.items?.map((item, i) => (
            <Box key={i} sx={{ display: 'flex', py: 1, borderBottom: '1px solid', borderColor: 'divider' }}>
              <Typography variant="body2" color="text.secondary" sx={{ width: '40%' }}>{item.key}</Typography>
              <Typography variant="body2">{item.value}</Typography>
            </Box>
          ))}
        </Box>
      );

    case 'list':
      return (
        <Box component="ul" sx={{ pl: 2, mb: 2 }}>
          {component.items?.map((item, i) => (
            <Typography component="li" key={i} variant="body2" sx={{ mb: 0.5 }}>
              {typeof item === 'string' ? item : item.label}
            </Typography>
          ))}
        </Box>
      );

    case 'code_block':
      return (
        <Paper sx={{ p: 2, mb: 2, bgcolor: 'grey.900' }}>
          <Typography component="pre" sx={{ fontFamily: 'monospace', fontSize: '0.875rem', color: 'grey.100', m: 0, overflow: 'auto' }}>
            {component.code}
          </Typography>
        </Paper>
      );

    default:
      return <Alert severity="warning" sx={{ mb: 2 }}>Unknown component: {(component as any).type}</Alert>;
  }
};
