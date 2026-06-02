Kernel config:
- make
```
CONFIG_USB_DWC3_PCI=y
CONFIG_USB_DWC3_QCOM=y
```

since otherwise they wont be loaded and usb-role-switch doesn't happen


- Delete vbus supply on usb_1, since even if we swap it to host mode as per carplay protocol, we don't want the driver
trying to feed the vehicle with power

then we set
```
+	qcom,vbus-valid-override;
+	qcom,vbus-override-value = <1>;
```

to ensure the vbus is still treated as valid, even without supply
