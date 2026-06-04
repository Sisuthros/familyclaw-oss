# FamilyClaw Blueprint - Sovereign Digital Kinship Engine  
  
Periaate: Ei kopioida yht„. Otetaan paras jokaisesta.  
  
Lahteet: OpenClaw, Hermes, Claw-code, Eternal Thread, Ironclaw, Smolagents, CrewAI, AutoGen, LangGraph  
  
Arkkitehtuuri: Rust core + Docker isolation + SurrealDB + VAD emotions + WASM sandbox + multi-channel  
  
14 crate-moduulia, 6-8 viikon toteutus, MIT-lisenssi  
  
KORJAUKSET:  
1. Resonance Bus: tokio::sync::broadcast + JSON vaiheessa 1  
2. SurrealDB Windows: agent_epsilon WSL2:ssa tai SQLite-fallback  
3. WASM: valitse YKSI runtime (Deno tai wasmtime)  
4. Luova autonomia: viittaus decision:077b28ee61ca3d1a  
5. Mallin vaihto: per-agentti konfiguraatio  
6. Kustannusbudjetti: vaiheet 1-3 ja tuotekehitys rinnakkain  
