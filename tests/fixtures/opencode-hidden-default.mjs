#!/usr/bin/env node
import process from 'node:process'

process.env.CAIRN_OPENCODE_FIXTURE_HIDDEN_DEFAULT = '1'
void import('./opencode.mjs')
