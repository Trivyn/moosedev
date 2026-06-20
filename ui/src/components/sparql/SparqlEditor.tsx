import { useEffect, useRef } from 'react';
import { Box } from '@mui/material';
import Yasqe from '@triply/yasqe';
import '@triply/yasqe/build/yasqe.min.css';

interface SparqlEditorProps {
  value: string;
  onChange: (value: string) => void;
}

export default function SparqlEditor({ value, onChange }: SparqlEditorProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const editorRef = useRef<Yasqe | null>(null);

  useEffect(() => {
    if (!containerRef.current || editorRef.current) return;
    const editor = new Yasqe(containerRef.current, {
      value,
      persistent: null,
      requestConfig: { endpoint: '' },
      syntaxErrorCheck: false,
    } as any);
    editor.on('change', () => onChange(editor.getValue()));
    editorRef.current = editor;
    return () => {
      editor.destroy();
      editorRef.current = null;
    };
    // Initialize once; external changes are synced below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (editorRef.current && editorRef.current.getValue() !== value) {
      editorRef.current.setValue(value);
    }
  }, [value]);

  return (
    <Box
      ref={containerRef}
      className="yasqe-container"
      sx={{ height: '100%', border: 1, borderColor: 'divider', borderRadius: 1, overflow: 'hidden' }}
    />
  );
}
