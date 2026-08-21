import {
  Alert,
  Box,
  Button,
  IconButton,
  Paper,
  Stack,
  TextField,
  Tooltip,
  Typography,
} from '@mui/material';
import ArrowDownwardIcon from '@mui/icons-material/ArrowDownward';
import ArrowUpwardIcon from '@mui/icons-material/ArrowUpward';
import PublishIcon from '@mui/icons-material/Publish';
import SaveOutlinedIcon from '@mui/icons-material/SaveOutlined';
import { StoryRecipe } from '../../api/types';
import {
  parseReferences,
  sectionKinds,
  sectionLabels,
  validateRecipe,
} from './storyModel';

interface ReferenceFieldProps {
  label: string;
  helperText: string;
  values: string[];
  disabled: boolean;
  onChange: (values: string[]) => void;
}

function ReferenceField({ label, helperText, values, disabled, onChange }: ReferenceFieldProps) {
  return (
    <TextField
      fullWidth
      size="small"
      disabled={disabled}
      label={label}
      helperText={helperText}
      value={values.join('\n')}
      onChange={(event) => onChange(parseReferences(event.target.value))}
      multiline
      minRows={3}
    />
  );
}

interface StoryEditorProps {
  recipe: StoryRecipe;
  busy: boolean;
  onChange: (recipe: StoryRecipe) => void;
  onSave: () => void;
  onPublish: () => void;
  onClose: () => void;
  dirty: boolean;
}

export default function StoryEditor({
  recipe,
  busy,
  onChange,
  onSave,
  onPublish,
  onClose,
  dirty,
}: StoryEditorProps) {
  const validationErrors = validateRecipe(recipe);
  const updateFocus = <K extends keyof StoryRecipe['focus']>(
    key: K,
    value: StoryRecipe['focus'][K],
  ) => {
    onChange({ ...recipe, focus: { ...recipe.focus, [key]: value } });
  };
  const moveEmphasis = (index: number, direction: -1 | 1) => {
    const destination = index + direction;
    if (destination < 0 || destination >= recipe.focus.emphasis.length) return;
    const emphasis = [...recipe.focus.emphasis];
    [emphasis[index], emphasis[destination]] = [emphasis[destination], emphasis[index]];
    updateFocus('emphasis', emphasis);
  };

  return (
    <Paper variant="outlined" sx={{ p: { xs: 2, md: 3 } }}>
      <Stack spacing={2.5}>
        <Stack direction="row" alignItems="center" justifyContent="space-between">
          <Box>
            <Typography variant="h6">Curate Story</Typography>
            <Typography variant="caption" color="text.secondary">
              Guide emphasis and evidence selection. Project claims remain in the knowledge graph.
            </Typography>
          </Box>
          <Button disabled={busy || dirty} onClick={onClose}>Close</Button>
        </Stack>
        <TextField
          disabled={busy}
          label="Title"
          value={recipe.title}
          onChange={(event) => onChange({ ...recipe, title: event.target.value })}
        />
        <TextField
          disabled
          label="Story subject"
          value={recipe.subject.type === 'entity' ? recipe.subject.iri : recipe.subject.query}
        />
        <TextField
          disabled={busy}
          label="Learning goal"
          value={recipe.goal}
          onChange={(event) => onChange({ ...recipe, goal: event.target.value })}
          multiline
        />

        <Box>
          <Typography variant="subtitle1">Narrative emphasis</Typography>
          <Typography variant="body2" color="text.secondary" sx={{ mb: 1 }}>
            Reorder the parts that deserve the most attention. This changes presentation, not evidence or chronology.
          </Typography>
          <Stack spacing={0.75}>
            {recipe.focus.emphasis.map((kind, index) => (
              <Stack key={kind} direction="row" alignItems="center" spacing={0.5}>
                <Tooltip title="Move up">
                  <span>
                    <IconButton
                      size="small"
                      aria-label={`Move ${sectionLabels[kind]} up`}
                      disabled={busy || index === 0}
                      onClick={() => moveEmphasis(index, -1)}
                    >
                      <ArrowUpwardIcon fontSize="small" />
                    </IconButton>
                  </span>
                </Tooltip>
                <Tooltip title="Move down">
                  <span>
                    <IconButton
                      size="small"
                      aria-label={`Move ${sectionLabels[kind]} down`}
                      disabled={busy || index === recipe.focus.emphasis.length - 1}
                      onClick={() => moveEmphasis(index, 1)}
                    >
                      <ArrowDownwardIcon fontSize="small" />
                    </IconButton>
                  </span>
                </Tooltip>
                <Typography variant="body2">{sectionLabels[kind]}</Typography>
              </Stack>
            ))}
            {recipe.focus.emphasis.length === 0 ? (
              <Button
                disabled={busy}
                variant="outlined"
                size="small"
                onClick={() => updateFocus('emphasis', sectionKinds)}
                sx={{ alignSelf: 'flex-start' }}
              >
                Use the standard emphasis
              </Button>
            ) : null}
          </Stack>
        </Box>

        <Box>
          <Typography variant="subtitle1" gutterBottom>Evidence focus</Typography>
          <Stack direction={{ xs: 'column', md: 'row' }} spacing={1.5}>
            <ReferenceField
              label="Include records"
              helperText="Promote subject-connected record IRIs, one per line"
              values={recipe.focus.include_record_iris}
              disabled={busy}
              onChange={(value) => updateFocus('include_record_iris', value)}
            />
            <ReferenceField
              label="Exclude records"
              helperText="Suppress from prose; lifecycle history remains honest"
              values={recipe.focus.exclude_record_iris}
              disabled={busy}
              onChange={(value) => updateFocus('exclude_record_iris', value)}
            />
          </Stack>
          <Stack direction={{ xs: 'column', md: 'row' }} spacing={1.5} sx={{ mt: 1.5 }}>
            <ReferenceField
              label="Include code symbols"
              helperText="Stable symbols to emphasize, one per line"
              values={recipe.focus.include_code_symbols}
              disabled={busy}
              onChange={(value) => updateFocus('include_code_symbols', value)}
            />
            <ReferenceField
              label="Exclude code symbols"
              helperText="Stable symbols to omit from the narrative"
              values={recipe.focus.exclude_code_symbols}
              disabled={busy}
              onChange={(value) => updateFocus('exclude_code_symbols', value)}
            />
          </Stack>
        </Box>

        <TextField
          disabled={busy}
          label="Curator context"
          placeholder="Optional guidance for readers; displayed as non-authoritative context"
          helperText={`${recipe.curator_context?.length ?? 0}/2000 characters`}
          value={recipe.curator_context ?? ''}
          onChange={(event) => onChange({ ...recipe, curator_context: event.target.value })}
          multiline
          minRows={3}
        />
        {validationErrors.length ? (
          <Alert severity="error">{validationErrors.join('. ')}.</Alert>
        ) : null}
        {dirty ? (
          <Alert severity="info">Save changes before closing or starting another Story.</Alert>
        ) : null}
        <Stack direction="row" spacing={1}>
          <Button
            variant="contained"
            startIcon={<SaveOutlinedIcon />}
            disabled={busy || validationErrors.length > 0}
            onClick={onSave}
          >
            {recipe.status === 'published' ? 'Save changes' : 'Save draft'}
          </Button>
          <Button
            variant="outlined"
            startIcon={<PublishIcon />}
            disabled={busy || validationErrors.length > 0}
            onClick={onPublish}
          >
            Publish
          </Button>
        </Stack>
      </Stack>
    </Paper>
  );
}
