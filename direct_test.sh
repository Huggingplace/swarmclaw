#!/bin/bash
export LLM_PROVIDER=gemini
export GEMINI_API_KEY=AIzaSyCjdhDGtmrqT-xP_TyfsDGJLe1zvWOkJcs
cd swarmclaw_core
cargo run --bin swarmclaw -- -w .. run < /dev/null
