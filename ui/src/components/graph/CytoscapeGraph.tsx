import { useEffect, useRef } from 'react';
import { Box, Typography } from '@mui/material';
import cytoscape from 'cytoscape';
import dagre from 'cytoscape-dagre';
import fcose from 'cytoscape-fcose';
import { GraphEdge, GraphNode } from '../../api/types';

cytoscape.use(dagre);
cytoscape.use(fcose);

interface CytoscapeGraphProps {
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export default function CytoscapeGraph({ nodes, edges }: CytoscapeGraphProps) {
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
            'background-color': '#1f6f5b',
            label: 'data(label)',
            color: '#1f2933',
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
            'line-color': '#8a9a9b',
            'target-arrow-color': '#8a9a9b',
            'target-arrow-shape': 'triangle',
            'curve-style': 'bezier',
            label: 'data(label)',
            'font-size': '9px',
            color: '#475569',
            'text-background-color': '#ffffff',
            'text-background-opacity': 0.8,
            'text-background-padding': '2px',
          },
        },
      ],
      wheelSensitivity: 0.18,
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
  }, [nodes, edges]);

  if (nodes.length === 0) {
    return (
      <Box sx={{ height: '100%', display: 'grid', placeItems: 'center', color: 'text.secondary' }}>
        <Typography variant="body2">No graph-shaped results.</Typography>
      </Box>
    );
  }

  return <Box ref={containerRef} sx={{ height: '100%', width: '100%' }} />;
}
