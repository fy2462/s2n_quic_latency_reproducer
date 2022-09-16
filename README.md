# s2n_quic_latency_reproducer

## Backgroud

There is the high latency wiht s2n_quic to send the data, the code is reproducer demo.

## Prepare

1. Make sure we have ssl dependency

    > sudo apt-get install openssl libssl-dev

2. Computer A run the demo server, listen the port 51111.

    > cd server && cargo run --release

3. Computer B run the demo client, connect the server IP, please modify the `SERVER_IP` field in `client/main.rs`.

    > cd client && cargo run --release

## Case 1

When the latency > 40ms from computer A to B with ping. 

![ping from A to B](./ping.png)

we send the image data with 25 fps. and output letancy about every 2 seconds. The latency(millisecond) is very high.

![case 1](./case_1.png)

## Case 2

All works well when the latency < 10ms from A to B.

However, When I specify the latency I want with `tc` command on computer B.

for example, I set up 40ms latency on network card `eth0` 

> sudo tc qdisc add dev eth0 root netem delay 50ms

We found the rapidly increasing network latency.

![case 2](./case_2.png)

## Expectation

We can get stable and as low as possible latency, even if there is some latency between A and B.
