import { describe, expect, it } from 'vitest';

import { QueryResponse } from '../../api/types';
import { queryToGraph, shortName } from './graphUtils';

describe('shortName', () => {
  it('uses known MOOSEDev prefixes before falling back to the last path segment', () => {
    expect(shortName('https://moosedev.dev/kg/project')).toBe('kg:project');
    expect(shortName('https://example.test/ns/Thing')).toBe('Thing');
  });
});

describe('queryToGraph', () => {
  it('turns subject/predicate/object bindings into Cytoscape nodes and edges', () => {
    const result: QueryResponse = {
      query_type: 'SELECT',
      head: { vars: ['subject', 'predicate', 'object'] },
      results: {
        bindings: [
          {
            subject: { type: 'uri', value: 'https://moosedev.dev/kg/A' },
            predicate: { type: 'uri', value: 'http://www.w3.org/2000/01/rdf-schema#seeAlso' },
            object: { type: 'uri', value: 'https://moosedev.dev/kg/B' },
          },
          {
            subject: { type: 'uri', value: 'https://moosedev.dev/kg/A' },
            predicate: { type: 'uri', value: 'http://www.w3.org/2000/01/rdf-schema#label' },
            object: { type: 'literal', value: 'A label' },
          },
        ],
      },
    };

    expect(queryToGraph(result)).toEqual({
      nodes: [
        {
          id: 'https://moosedev.dev/kg/A',
          label: 'A label',
          type: 'projectRecord',
          properties: [
            {
              predicate: 'http://www.w3.org/2000/01/rdf-schema#label',
              values: [{ type: 'literal', value: 'A label' }],
            },
            {
              predicate: 'http://www.w3.org/2000/01/rdf-schema#seeAlso',
              values: [{ type: 'uri', value: 'https://moosedev.dev/kg/B' }],
            },
          ],
        },
        {
          id: 'https://moosedev.dev/kg/B',
          label: 'kg:B',
          type: 'projectRecord',
          properties: [
            {
              predicate: 'urn:moosedev:incomingPredicate',
              values: [{ type: 'uri', value: 'http://www.w3.org/2000/01/rdf-schema#seeAlso' }],
            },
          ],
        },
      ],
      edges: [
        {
          id: 'https://moosedev.dev/kg/A|http://www.w3.org/2000/01/rdf-schema#seeAlso|https://moosedev.dev/kg/B|0',
          source: 'https://moosedev.dev/kg/A',
          target: 'https://moosedev.dev/kg/B',
          label: 'rdfs:seeAlso',
          type: 'rdfs:seeAlso',
          predicate: 'http://www.w3.org/2000/01/rdf-schema#seeAlso',
          properties: [
            {
              predicate: 'urn:moosedev:predicate',
              values: [{ type: 'uri', value: 'http://www.w3.org/2000/01/rdf-schema#seeAlso' }],
            },
          ],
        },
      ],
    });
  });

  it('also understands conventional s/p/o bindings', () => {
    const result: QueryResponse = {
      query_type: 'SELECT',
      head: { vars: ['s', 'p', 'o'] },
      results: {
        bindings: [
          {
            s: { type: 'uri', value: 'https://moosedev.dev/kg/A' },
            p: { type: 'uri', value: 'http://www.w3.org/2000/01/rdf-schema#seeAlso' },
            o: { type: 'uri', value: 'https://moosedev.dev/kg/B' },
          },
        ],
      },
    };

    expect(queryToGraph(result).edges).toHaveLength(1);
  });

  it('classifies MOOSE execution and stage-run resources as trace nodes', () => {
    const result: QueryResponse = {
      query_type: 'CONSTRUCT',
      triples: [
        {
          subject: { type: 'uri', value: 'https://moosedev.dev/kg/session/execution/1' },
          predicate: { type: 'uri', value: 'https://moosedev.dev/kg/ranStage' },
          object: { type: 'uri', value: 'https://moosedev.dev/kg/session/stage-run/2' },
        },
      ],
    };

    expect(queryToGraph(result).nodes.map((node) => node.type)).toEqual(['mooseTrace', 'mooseTrace']);
  });

  it('can hide MOOSE trace nodes and their connected edges', () => {
    const result: QueryResponse = {
      query_type: 'CONSTRUCT',
      triples: [
        {
          subject: { type: 'uri', value: 'https://moosedev.dev/kg/A' },
          predicate: { type: 'uri', value: 'https://moosedev.dev/kg/relatesTo' },
          object: { type: 'uri', value: 'https://moosedev.dev/kg/B' },
        },
        {
          subject: { type: 'uri', value: 'https://moosedev.dev/kg/A' },
          predicate: { type: 'uri', value: 'https://moosedev.dev/kg/hasExecution' },
          object: { type: 'uri', value: 'https://moosedev.dev/kg/session/execution/1' },
        },
        {
          subject: { type: 'uri', value: 'https://moosedev.dev/kg/session/execution/1' },
          predicate: { type: 'uri', value: 'https://moosedev.dev/kg/ranStage' },
          object: { type: 'uri', value: 'https://moosedev.dev/kg/session/stage-run/2' },
        },
      ],
    };

    const graph = queryToGraph(result, { showMooseTraces: false });

    expect(graph.nodes.map((node) => node.id)).toEqual(['https://moosedev.dev/kg/A', 'https://moosedev.dev/kg/B']);
    expect(graph.edges).toHaveLength(1);
    expect(graph.edges[0].label).toBe('kg:relatesTo');
  });
});
