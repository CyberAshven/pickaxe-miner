// pickaxe Stage A placeholder — compile with nvcc once toolkit is installed.
// Real PHOTON transcript must match postcorps reference / BCH Schnorr challenge:
//   e = SHA256(r_x || pubkey || msg)
extern "C" __global__ void pickaxe_stage_a_placeholder(const unsigned int *nonce_base,
                                                       unsigned int *out_count) {
    unsigned int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i == 0 && out_count) {
        *out_count = *nonce_base;
    }
}
