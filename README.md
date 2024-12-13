# CS7344 - Project 2

## Summary

This is a small sample of a 'game' sending many tiny packets over different IP protocols in order to compare performance. The client application creates tasks for 100 players. When each connection is established, the server replays the current player state. The player task then sends 100 'moves', each of which is processed by the server, then re-broadcast to all current connections. The client application waits for all events to be received, then compares all player tasks' data to ensure they share a common state.

## Results

TCP (no encryption)
![image](https://github.com/user-attachments/assets/072de582-6691-45d0-947f-7f7491e2fdb4)

TCP (with encryption)
![image](https://github.com/user-attachments/assets/719f3ca2-15f3-469e-8efd-e2d087e947ee)

UDP
![image](https://github.com/user-attachments/assets/7d2cc040-2ec9-4485-ba0c-ab511d917ac8)

## Environment

|           |                                              |
|-----------|----------------------------------------------|
| Hardware  | Lenovo Legion S7 15ACH6                      |
| Memory    | 16.0 GiB                                     |
| Processor | AMD® Ryzen 7 5800h with radeon graphics × 16 |
| OS        | Pop!_OS 22.04 LTS; 64 bit                    |

Rust version: 1.83.0

The binaries were produced using `cargo build`; the client with a 'profiling' profile (release + debug), and the server with the release profile.
