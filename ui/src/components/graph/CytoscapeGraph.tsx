import { useEffect, useMemo, useRef, useState } from 'react';
import { Box, Divider, Menu, MenuItem, Typography } from '@mui/material';
import { useTheme } from '@mui/material/styles';
import cytoscape from 'cytoscape';
import dagre from 'cytoscape-dagre';
import fcose from 'cytoscape-fcose';
import { GraphEdge, GraphNode } from '../../api/types';
import GraphDetailsPanel from './GraphDetailsPanel';

cytoscape.use(dagre);
cytoscape.use(fcose);

// Cytoscape's default wheel zoom is conservative; this keeps graph exploration responsive.
const WHEEL_SENSITIVITY = 0.45;

interface GraphMenuState {
  x: number;
  y: number;
  targetType: 'node' | 'edge' | 'core';
  targetId?: string;
}

export interface CytoscapeGraphProps {
  nodes: GraphNode[];
  edges: GraphEdge[];
  mode?: 'explore' | 'navigate';
  focusNodeId?: string;
  onNodeClick?: (node: GraphNode) => void;
}

interface LegendItem {
  label: string;
  color: string;
  shape: 'ellipse' | 'round-rectangle' | 'tag' | 'diamond' | 'rectangle';
}

export default function CytoscapeGraph({
  nodes,
  edges,
  mode = 'explore',
  focusNodeId,
  onNodeClick,
}: CytoscapeGraphProps) {
  const theme = useTheme();
  const containerRef = useRef<HTMLDivElement | null>(null);
  const cyRef = useRef<cytoscape.Core | null>(null);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [selectedEdgeId, setSelectedEdgeId] = useState<string | null>(null);
  const [menu, setMenu] = useState<GraphMenuState | null>(null);
  const nodeById = useMemo(() => new Map(nodes.map((node) => [node.id, node])), [nodes]);
  const edgeById = useMemo(() => new Map(edges.map((edge) => [edge.id, edge])), [edges]);
  const selectedNode = selectedNodeId ? nodeById.get(selectedNodeId) : null;
  const selectedEdge = selectedEdgeId ? edgeById.get(selectedEdgeId) : null;
  const legendItems: LegendItem[] = useMemo(
    () => [
      { label: 'Project record', color: theme.palette.primary.main, shape: 'round-rectangle' },
      { label: 'MOOSE trace', color: theme.palette.success.main, shape: 'tag' },
      { label: 'Ontology/schema', color: theme.palette.info.main, shape: 'diamond' },
      { label: 'Blank node', color: theme.palette.warning.main, shape: 'rectangle' },
      { label: 'Other', color: theme.palette.grey[500], shape: 'ellipse' },
    ],
    [theme],
  );

  useEffect(() => {
    if (!containerRef.current) return;
    const elements: cytoscape.ElementDefinition[] = [
      ...nodes.map((node) => ({
        classes: node.id === focusNodeId ? 'focus-node' : undefined,
        data: {
          id: node.id,
          label: node.label,
          type: node.type,
          properties: node.properties ?? [],
        },
      })),
      ...edges.map((edge) => ({
        data: {
          id: edge.id,
          source: edge.source,
          target: edge.target,
          label: edge.label,
          type: edge.type,
          predicate: edge.predicate,
          properties: edge.properties ?? [],
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
            'background-color': theme.palette.grey[500],
            label: 'data(label)',
            color: theme.palette.text.primary,
            'font-size': '11px',
            'min-zoomed-font-size': 8,
            'text-wrap': 'wrap',
            'text-max-width': '120px',
            'text-valign': 'bottom',
            'text-margin-y': 6,
            'text-outline-width': theme.palette.mode === 'dark' ? 2 : 0,
            'text-outline-color': theme.palette.background.default,
            width: 28,
            height: 28,
            shape: 'ellipse',
          },
        },
        {
          selector: 'node[type="projectRecord"]',
          style: {
            'background-color': theme.palette.primary.main,
            shape: 'round-rectangle',
            width: 34,
            height: 28,
          },
        },
        {
          selector: 'node[type="mooseTrace"]',
          style: {
            'background-color': theme.palette.success.main,
            shape: 'tag',
            width: 42,
            height: 30,
          },
        },
        {
          selector: 'node[type="ontology"], node[type="schema"]',
          style: {
            'background-color': theme.palette.info.main,
            shape: 'diamond',
            width: 34,
            height: 34,
          },
        },
        {
          selector: 'node[type="bnode"]',
          style: {
            'background-color': theme.palette.warning.main,
            shape: 'rectangle',
          },
        },
        {
          selector: 'node.focus-node',
          style: {
            'border-width': 4,
            'border-color': theme.palette.secondary.main,
          },
        },
        {
          selector: 'node.user-selected, edge.user-selected',
          style: {
            'border-width': 4,
            'border-color': theme.palette.secondary.main,
            'overlay-color': theme.palette.secondary.main,
            'overlay-opacity': 0.18,
            'overlay-padding': 8,
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

    const selectElement = (element: cytoscape.NodeSingular | cytoscape.EdgeSingular) => {
      cy.elements().removeClass('user-selected');
      element.addClass('user-selected');
      setMenu(null);
      if (element.isNode()) {
        setSelectedNodeId(element.id());
        setSelectedEdgeId(null);
      } else if (element.isEdge()) {
        setSelectedEdgeId(element.id());
        setSelectedNodeId(null);
      }
    };

    const openMenu = (event: cytoscape.EventObject, targetType: GraphMenuState['targetType']) => {
      event.preventDefault();
      const position = event.renderedPosition ?? { x: 0, y: 0 };
      const rect = containerRef.current?.getBoundingClientRect();
      setMenu({
        x: (rect?.left ?? 0) + position.x,
        y: (rect?.top ?? 0) + position.y,
        targetType,
        targetId: targetType === 'core' ? undefined : event.target.id(),
      });
    };

    if (mode === 'navigate') {
      cy.on('tap', 'node', (event) => {
        const node = nodeById.get(event.target.id());
        if (node) onNodeClick?.(node);
      });
    } else {
      cy.on('tap', 'node, edge', (event) => selectElement(event.target));
    }
    cy.on('tap', (event) => {
      if (event.target === cy) setMenu(null);
    });
    if (mode === 'explore') {
      cy.on('cxttap', 'node', (event) => openMenu(event, 'node'));
      cy.on('cxttap', 'edge', (event) => openMenu(event, 'edge'));
      cy.on('cxttap', (event) => {
        if (event.target === cy) openMenu(event, 'core');
      });
    }

    return () => {
      cy.destroy();
      cyRef.current = null;
    };
  }, [nodes, edges, focusNodeId, mode, nodeById, onNodeClick, theme]);

  useEffect(() => {
    setSelectedNodeId((id) => (id && nodeById.has(id) ? id : null));
    setSelectedEdgeId((id) => (id && edgeById.has(id) ? id : null));
  }, [edgeById, nodeById]);

  const closeMenu = () => setMenu(null);

  const runLayout = (name: cytoscape.LayoutOptions['name']) => {
    closeMenu();
    cyRef.current
      ?.layout({
        name,
        fit: true,
        padding: 35,
        animate: false,
      } as cytoscape.LayoutOptions)
      .run();
  };

  const showAll = () => {
    closeMenu();
    const cy = cyRef.current;
    if (!cy) return;
    cy.elements().style('display', 'element');
    cy.elements().removeClass('user-selected');
    setSelectedNodeId(null);
    setSelectedEdgeId(null);
    cy.fit(undefined, 35);
  };

  const hideSelected = () => {
    closeMenu();
    if (!menu?.targetId) return;
    const element = cyRef.current?.getElementById(menu.targetId);
    element?.style('display', 'none');
  };

  const isolateSelectedNode = () => {
    closeMenu();
    if (!menu?.targetId || menu.targetType !== 'node') return;
    const cy = cyRef.current;
    const node = cy?.getElementById(menu.targetId);
    if (!cy || !node) return;
    const visible = node.closedNeighborhood();
    cy.elements().style('display', 'none');
    visible.style('display', 'element');
    cy.fit(visible, 35);
  };

  const hideHighDegreeNodes = () => {
    closeMenu();
    const cy = cyRef.current;
    if (!cy) return;
    cy.nodes().forEach((node) => {
      if (node.degree() > 10) {
        node.style('display', 'none');
        node.connectedEdges().style('display', 'none');
      }
    });
  };

  const fitGraph = () => {
    closeMenu();
    cyRef.current?.fit(undefined, 35);
  };

  if (nodes.length === 0) {
    return (
      <Box sx={{ height: '100%', display: 'grid', placeItems: 'center', color: 'text.secondary' }}>
        <Typography variant="body2">No graph-shaped results.</Typography>
      </Box>
    );
  }

  return (
    <Box sx={{ height: '100%', width: '100%', position: 'relative' }}>
      <Box ref={containerRef} sx={{ height: '100%', width: '100%' }} />
      {mode === 'explore' && <Box
        aria-label="Graph node legend"
        sx={{
          position: 'absolute',
          left: 10,
          top: 10,
          zIndex: 1,
          px: 1,
          py: 0.75,
          display: 'flex',
          flexDirection: 'column',
          gap: 0.5,
          bgcolor: (currentTheme) =>
            currentTheme.palette.mode === 'dark' ? 'rgba(24, 32, 32, 0.9)' : 'rgba(255, 255, 255, 0.92)',
          border: 1,
          borderColor: 'divider',
          borderRadius: 1,
          boxShadow: 1,
          pointerEvents: 'none',
        }}
      >
        {legendItems.map((item) => (
          <Box key={item.label} sx={{ display: 'flex', alignItems: 'center', gap: 0.75 }}>
            <Box
              sx={{
                width: 12,
                height: 12,
                bgcolor: item.color,
                borderRadius:
                  item.shape === 'ellipse'
                    ? '50%'
                    : item.shape === 'round-rectangle' || item.shape === 'tag'
                      ? 0.75
                      : 0,
                transform: item.shape === 'diamond' ? 'rotate(45deg)' : 'none',
                clipPath:
                  item.shape === 'tag'
                    ? 'polygon(0 0, 75% 0, 100% 50%, 75% 100%, 0 100%)'
                    : 'none',
                border: (currentTheme) => `1px solid ${currentTheme.palette.divider}`,
              }}
            />
            <Typography variant="caption" color="text.secondary" sx={{ lineHeight: 1.2 }}>
              {item.label}
            </Typography>
          </Box>
        ))}
      </Box>}
      {mode === 'explore' && <GraphDetailsPanel node={selectedNode} edge={selectedEdge} onClose={() => {
        cyRef.current?.elements().removeClass('user-selected');
        setSelectedNodeId(null);
        setSelectedEdgeId(null);
      }} />}
      {mode === 'explore' && <Menu
        open={Boolean(menu)}
        onClose={closeMenu}
        anchorReference="anchorPosition"
        anchorPosition={menu ? { top: menu.y, left: menu.x } : undefined}
      >
        {menu?.targetType === 'node' && <MenuItem onClick={isolateSelectedNode}>Isolate Node</MenuItem>}
        {menu?.targetType !== 'core' && <MenuItem onClick={hideSelected}>Hide Selected</MenuItem>}
        {(menu?.targetType === 'node' || menu?.targetType === 'edge') && <Divider />}
        <MenuItem onClick={showAll}>Show All</MenuItem>
        <MenuItem onClick={hideHighDegreeNodes}>Hide High-Degree Nodes</MenuItem>
        <Divider />
        <MenuItem onClick={fitGraph}>Fit Graph</MenuItem>
        <MenuItem onClick={() => runLayout(nodes.length > 80 ? 'fcose' : 'dagre')}>Auto Layout</MenuItem>
        <MenuItem onClick={() => runLayout('fcose')}>CoSE Layout</MenuItem>
        <MenuItem onClick={() => runLayout('dagre')}>Hierarchy Layout</MenuItem>
        <MenuItem onClick={() => runLayout('circle')}>Circle Layout</MenuItem>
        <MenuItem onClick={() => runLayout('grid')}>Grid Layout</MenuItem>
      </Menu>}
    </Box>
  );
}
