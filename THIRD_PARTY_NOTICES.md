# Third-Party Notices

This repository (`ui-automata-linux`) incorporates MIT-licensed components from `visioncortex/ui-automata` (see crate `LICENSE` files) and references architectural patterns and selectively adapted modules from the following open-source projects under their respective licenses:

1. **agent-sh/computer-use-linux** (MIT License)  
   - Repository: https://github.com/agent-sh/computer-use-linux  
   - Copyright (c) agent-sh contributors  
   - Referenced/adapted patterns in `automata-linux` & `automata-agent`: bounded `CommandRunner` (`ETXTBSY` retry / timeout handling), 53-bit JS-safe window ID constraints, MCP HTTP security middleware, concurrent/bounded AT-SPI2 tree traversal & compact `U+FFFC` node filtering, Qt/GTK `EditableText` readback verification, multi-compositor windowing backends, and `RemoteDesktop + EIS` input design.

2. **enigo-rs/enigo** (MIT License)  
   - Repository: https://github.com/enigo-rs/enigo  
   - Referenced for Linux XKB / `x11rb` / `libei` input integration patterns.

3. **bilelmoussaoui/ashpd** (MIT License)  
   - Repository: https://github.com/bilelmoussaoui/ashpd  
   - Referenced for XDG Desktop Portal D-Bus interaction patterns.
