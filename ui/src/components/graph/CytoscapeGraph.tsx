import { useEffect, useRef } from 'react';
import { Box, Typography } from '@mui/material';
import { useTheme } from '@mui/material/styles';
import cytoscape from 'cytoscape';
import dagre from 'cytoscape-dagre';
import fcose from 'cytoscape-fcose';
import { GraphEdge, GraphNode } from '../../api/types';

cytoscape.use(dagre);
cytoscape.use(fcose);

// Cytoscape's default wheel zoom is conservative; this keeps graph exploration responsive.
const WHEEL_SENSITIVITY = 0.45;

interface CytoscapeGraphProps {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export default function CytoscapeGraph({ nodes, edges }: CytoscapeGraphProps) {
  const theme = useTheme();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const cyRef = useRef<cytoscape.Core | null>(null);

  useEffect(() => {
    if (!containerRef.current) return;
    const elements: cytoscape.ElementDefinition[] = [
      ...nodes.map((node) => ({
        data: {
          id: node.id,
          label: node.label,
          type: node.type,
        },
      })),
      ...edges.map((edge) => ({
        data: {
          id: edge.id,
          source: edge.source,
          target: edge.target,
          label: edge.label,
          type: edge.type,
        },
      })),
    ];

    const cy = cytoscape({
      container: containerRef.current,
      elements,
      style: [
        {
          selector: 'node',
          style: {
            'background-color': theme.palette.primary.main,
            label: 'data(label)',
            color: theme.palette.text.primary,
            'font-size': '10px',
            'text-wrap': 'wrap',
            'text-max-width': '120px',
            'text-valign': 'bottom',
            'text-margin-y': 6,
            width: 18,
            height: 18,
          },
        },
        {
          selector: 'edge',
          style: {
            width: 1.5,
            'line-color': theme.palette.text.disabled,
            'target-arrow-color': theme.palette.text.disabled,
            'target-arrow-shape': 'triangle',
            'curve-style': 'bezier',
            label: 'data(label)',
            'font-size': '9px',
            color: theme.palette.text.secondary,
            'text-background-color': theme.palette.background.default,
            'text-background-opacity': 0.8,
            'text-background-padding': '2px',
          },
        },
      ],
      wheelSensitivity: WHEEL_SENSITIVITY,
    });
    cyRef.current = cy;
    const layoutName = nodes.length > 80 ? 'fcose' : 'dagre';
    cy.layout({
      name: layoutName,
      fit: true,
      padding: 35,
      animate: false,
    } as cytoscape.LayoutOptions).run();

    return () => {
      cy.destroy();
      cyRef.current = null;
    };
  }, [nodes, edges, theme]);

  if (nodes.length === 0) {
    return (
      <Box sx={{ height: '100%', display: 'grid', placeItems: 'center', color: 'text.secondary' }}>
        <Typography variant="body2">No graph-shaped results.</Typography>
      </Box>
    );
  }

  return <Box ref={containerRef} sx={{ height: '100%', width: '100%' }} />;
}
