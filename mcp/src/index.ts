#!/usr/bin/env node
/**
 * `framekeep-mcp` -- the MCP adapter.
 *
 * A shell over `framekeep-core`. Nothing here knows how to decode a video; it
 * knows the protocol, the reply budget, and which channel reaches the model.
 *
 * Two tools, not three. Speech arrives long after frames do, and the obvious
 * fix is a `video_transcript` tool to poll -- but every extra tool is another
 * thing the model has to learn to choose correctly, and `AGENTS.md` is blunt
 * about what a badly-chosen tool costs. `video_map` is cheap and side-effect
 * free, so calling it again IS the way to ask whether the words are ready.
 */

import { Server } from '@modelcontextprotocol/sdk/server/index.js';
import { StdioServerTransport } from '@modelcontextprotocol/sdk/server/stdio.js';
import {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} from '@modelcontextprotocol/sdk/types.js';

import { DESCRIPTION as MAP_DESC, videoMap } from './tools/video_map.js';
import { DESCRIPTION as FRAMES_DESC, videoFrames } from './tools/video_frames.js';
import { bothChannels } from './channels.js';
import { runInit } from './init.js';

// `init` is run by a person in a terminal; everything else is a client speaking
// MCP over stdio. Checked before the transport is opened, because a server that
// starts talking JSON-RPC at someone typing a command is not helping.
if (process.argv[2] === 'init') {
  process.exit(runInit(process.argv.slice(3)));
}

const server = new Server(
  { name: 'framekeep', version: '0.1.0' },
  { capabilities: { tools: {} } },
);

/**
 * Which client we are talking to, learned from the handshake.
 *
 * Used only to size the reply budget. Read through a getter rather than
 * captured once, because the handshake finishes after this module is loaded.
 */
function clientName(): string | undefined {
  return server.getClientVersion()?.name;
}

server.setRequestHandler(ListToolsRequestSchema, async () => ({
  tools: [
    {
      name: 'video_map',
      description: MAP_DESC,
      inputSchema: {
        type: 'object',
        properties: {
          path: {
            type: 'string',
            description: 'Full path to the screen recording on this machine.',
          },
        },
        required: ['path'],
      },
    },
    {
      name: 'video_frames',
      description: FRAMES_DESC,
      inputSchema: {
        type: 'object',
        properties: {
          path: { type: 'string', description: 'Full path to the screen recording.' },
          from: { type: 'number', description: 'Start of the range, in seconds.' },
          to: { type: 'number', description: 'End of the range, in seconds.' },
          max_frames: {
            type: 'number',
            description:
              'Upper bound on frames returned. The reply budget usually binds ' +
              'first, so a large number here does not get you more.',
          },
          region: {
            type: 'array',
            items: { type: 'number' },
            minItems: 4,
            maxItems: 4,
            description:
              '[x1, y1, x2, y2] in the pixels of the recording itself. Crops every ' +
              'frame to that rectangle at full resolution -- nothing is ' +
              'shrunk. A smaller area fits several times more frames in one ' +
              'reply, so this is the cheapest way to see more of a recording.',
          },
          output_mode: {
            type: 'string',
            enum: ['auto', 'images', 'files', 'text'],
            description:
              'How to deliver the frames. `images` (the default) puts them in ' +
              'the reply. `files` returns paths for you to open yourself, which ' +
              'the reply-size limit does not apply to -- not available where the ' +
              'person hid something. `text` returns no pictures at all, only ' +
              'which frames exist and when.',
          },
        },
        required: ['path'],
      },
    },
  ],
}));

server.setRequestHandler(CallToolRequestSchema, async (req) => {
  const args = (req.params.arguments ?? {}) as Record<string, unknown>;
  const path = args.path;

  if (typeof path !== 'string' || path.length === 0) {
    return failure('This tool needs `path`: the full path to a screen recording on this machine.');
  }

  try {
    switch (req.params.name) {
      case 'video_map':
        return await videoMap({ path });

      case 'video_frames':
        return await videoFrames(
          {
            path,
            from: numberOr(args.from),
            to: numberOr(args.to),
            max_frames: numberOr(args.max_frames),
            region: Array.isArray(args.region) ? (args.region as number[]) : undefined,
            // Passed through unvalidated on purpose: the tool refuses an
            // unknown mode by name, and a value quietly coerced here would
            // reach it as `auto` and never be reported as wrong.
            output_mode: args.output_mode,
          },
          clientName(),
        );

      default:
        return failure(`No such tool: ${req.params.name}. This server offers video_map and video_frames.`);
    }
  } catch (e) {
    // core's own errors already say what broke and what to do next, so they are
    // passed through rather than replaced with something vaguer.
    return failure(e instanceof Error ? e.message : String(e));
  }
});

function numberOr(v: unknown): number | undefined {
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined;
}

/**
 * An error the model can act on.
 *
 * Sent down both channels like everything else: on one measured client prose
 * beside structuredContent is dropped, and an error message that silently
 * vanishes is worse than no error at all.
 */
function failure(message: string) {
  return {
    ...bothChannels({ instructions: [message], data: { error: message } }),
    isError: true,
  };
}

await server.connect(new StdioServerTransport());
