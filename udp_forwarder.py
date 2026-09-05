import socket

sock_in = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock_out = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)

sock_in.bind(("0.0.0.0", 5005))

TARGET_IP = "172.22.100.172"
TARGET_PORT = 5005

print("UDP Forwarder listening 5005")
print(f"Forwarding to {TARGET_IP}:{TARGET_PORT}")

while True:
    data, addr = sock_in.recvfrom(65535)
    print(f"RX {len(data)} bytes from {addr}")
    sock_out.sendto(data, (TARGET_IP, TARGET_PORT))
